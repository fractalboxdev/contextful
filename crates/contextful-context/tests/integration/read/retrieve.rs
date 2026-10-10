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

/// Without the lexical backend linked, ranking substitutes a token fallback scoring each query token over the snippet, recency breaking ties. The result differs in order and still answers.
// spec: read.rank.degradation-not-error@14477b43
#[cfg(not(feature = "fts"))]
#[test]
fn without_the_lexical_backend_a_ranked_read_answers_by_token_fallback() {
    assert!(!contextful_context::read::LEXICAL_BACKEND);
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    let ranked = r.face.retrieve(&s, &ask("research/notes", "solar battery storage"), Bounds::default()).unwrap();
    assert!(ranked.blocks["contextful.retrieval"]["matched"].as_u64().unwrap() > 0, "{:?}", ranked.blocks);
    let scores: Vec<u64> = column(&ranked, "_score").iter().map(|s| s.as_u64().unwrap()).collect();
    assert!(!scores.is_empty());
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{scores:?}");
    // n1 and n4 match every token; the later publication leads.
    let order = ids(&ranked, "note_id");
    let (n4, n1) = (order.iter().position(|i| i == "n4").unwrap(), order.iter().position(|i| i == "n1").unwrap());
    assert!(n4 < n1, "{order:?}");
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
    assert!(!column(&quiet, "_date_basis").is_empty(), "the exclusion below ranges over no element");
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
    for c in ["_modality", "_lang", "_provenance", "_prompt_hash", "_kind"] {
        assert!(!column(&ranked, c).is_empty(), "the exclusion below ranges over no element");
        assert!(column(&ranked, c).iter().all(|v| v.is_null()), "{c}");
    }
}

/// A producer sets any of `_modality`, `_lang`, `_provenance` and `_prompt_hash`, and each surfaces in the provenance envelope where present.
// spec: store.reserve.optional@acdb41f5
#[test]
fn optional_provenance_columns_surface_in_ranked_rows() {
    let r = Reads::new();
    let hash = format!("sha256:{}", "0".repeat(64));
    let provenance = r#"[{"table":"research/notes","key":{"note_id":"n1"}}]"#;
    land_rows(
        &r.store,
        "research/notes",
        "run-0002",
        json!([{"note_id": "n5", "tenant": "acme", "title": "Provenance specimen", "_modality": "text", "_lang": "en-GB", "_provenance": provenance, "_prompt_hash": hash}]),
    );
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let ranked = r.face.retrieve(&s, &ask("research/notes", "specimen"), Bounds::default()).unwrap();
    assert_eq!(ids(&ranked, "note_id"), ["n5"]);
    for (name, expected) in [
        ("_modality", json!("text")),
        ("_lang", json!("en-GB")),
        ("_provenance", json!(provenance)),
        ("_prompt_hash", json!(hash)),
    ] {
        assert_eq!(column(&ranked, name), [expected], "{name}");
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

/// Byte truncation leaves the retrieval counts equal to the rows actually delivered.
// spec: read.rank.delivered-counts@61945ab5
#[test]
fn a_byte_cut_updates_the_retrieval_counts() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let request = ask("research/", "solar battery storage");
    let full = r.face.retrieve(&s, &request, Bounds::default()).unwrap();
    assert_eq!(full.rows.len(), 2);
    let mut one = full.clone();
    one.rows.truncate(1);
    one.truncated = true;
    one.blocks["contextful.retrieval"]["returned"] = json!(1);
    one.blocks["contextful.retrieval"]["in_window"] = one.rows[0][one.columns.iter().position(|c| c == "_in_window").unwrap()].clone();
    one.blocks.insert("contextful.truncation".into(), json!({ "by": "bytes", "ceiling": 999999, "source": "request" }));
    let ceiling = serde_json::to_vec(&one).unwrap().len() as u64;
    let cut = r.face.retrieve(&s, &RetrieveRequest { max_response_bytes: Some(ceiling), ..request }, Bounds::default()).unwrap();
    assert_eq!(cut.rows.len(), 1);
    let block = &cut.blocks["contextful.retrieval"];
    assert_eq!(block["returned"], json!(1));
    let flags = column(&cut, "_in_window").into_iter().filter(|v| v == &json!(true)).count();
    assert_eq!(block["in_window"], json!(flags));
}

/// The lexical engine's own float score never crosses to a caller.
// spec: read.rank.internal-score-stays-internal@8a3741ac
#[test]
fn only_the_integer_score_crosses() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let ranked = r.face.retrieve(&s, &ask("research/", "solar battery storage"), Bounds::default()).unwrap();
    assert_eq!(column(&ranked, "_score"), [json!(3), json!(2)]);
    assert!(!column(&ranked, "_vscore").is_empty(), "the exclusion below ranges over no element");
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
    assert!(huge.blocks["contextful.retrieval"]["window"].as_u64().unwrap() <= 8 * contextful_core::read::respond::FACE_ROW_CEILING);
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
    sidecar_reads_with_rows(extra, |i| match i {
        0 => ("passage zero".to_string(), json!([0.0, 0.0, 1.0])),
        295.. => (format!("battery cell {i}"), json!([1.0, (i % 10) as f64 / 10.0, 0.0])),
        _ => (format!("passage {i}"), json!([1.0, (i % 10) as f64 / 10.0, 0.0])),
    })
}

fn sidecar_reads_with_rows(extra: &str, row_for: impl Fn(usize) -> (String, Value)) -> Reads {
    use contextful_context::fold::fold;
    use contextful_core::store::reconcile::{ColumnType, FloatItem};
    let manifest = format!("{MANIFEST}{SIDECAR}{extra}");
    let mut r = Reads::with_manifest(&format!("{MANIFEST}{SIDECAR}"));
    let rows: Vec<serde_json::Map<String, Value>> = (0..300)
        .map(|i| {
            let (title, embedding) = row_for(i);
            let owner = if i == 0 { "agent://other" } else { "agent://research-loop" };
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

/// `lab/indexed` alone, under a row policy keyed on `owner`: `p0000`, the oldest row and
/// the reader's own, points near the question; `crowd` rows of another agent point nearer
/// still; 299 newer rows of the reader's point elsewhere and fill the recency window.
fn crowded_reads(crowd: usize) -> Reads {
    use contextful_context::fold::fold;
    use contextful_core::store::reconcile::{ColumnType, FloatItem};
    let manifest = format!("{MANIFEST}{SIDECAR}[pipeline.tables.policy.rows]\npredicate = \"owner = subject.agent\"\n");
    let mut r = Reads::with_manifest(&format!("{MANIFEST}{SIDECAR}"));
    let rows: Vec<serde_json::Map<String, Value>> = (0..crowd + 300)
        .map(|i| {
            let (owner, embedding) = match i {
                0 => ("agent://research-loop", json!([0.05, 0.0, 1.0])),
                i if i <= crowd => ("agent://other", json!([(i % 7) as f64 / 1000.0, (i % 11) as f64 / 1000.0, 1.0])),
                i => ("agent://research-loop", json!([1.0, (i % 10) as f64 / 10.0, 0.0])),
            };
            json!({"passage_id": format!("p{i:04}"), "title": format!("passage {i}"), "owner": owner, "embedding": embedding}).as_object().unwrap().clone()
        })
        .collect();
    let types: HashMap<String, ColumnType> = [("embedding".to_string(), ColumnType::FixedSizeList(FloatItem::Float32, 3))].into_iter().collect();
    let decl = TableDecl::parse_pipeline(&manifest).unwrap().into_iter().find(|d| d.name == "lab/indexed").unwrap();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-0001".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at("2030-01-10T00:00:00Z"),
    };
    land(&r.store, &decl, &Batch { rows, types }, &ctx).unwrap();
    fold(&r.store, &decl, at("2030-01-11T00:00:00Z")).unwrap();
    r.face = Face::open(r.store.clone(), &manifest, pepper()).unwrap();
    r
}

/// Where the rows a reader can see under-fill the requested limit, the probe doubles its size and probes again, for at most 4 rounds, then answers and reports the under-fill in the retrieval block.
// spec: read.retrieve.adaptive-over-fetch@5399f76a
#[test]
fn an_under_filled_probe_doubles_until_the_reader_sees_its_rows() {
    let question = |table: &str| RetrieveRequest { query_embedding: Some(vec![0.0, 0.0, 1.0]), limit: Some(5), ..ask(table, "") };
    // 600 rows the reader cannot see sit nearer than its own: the first 256-row probe and
    // the 512-row second miss `p0000`, the 1024-row third reaches it.
    let r = crowded_reads(600);
    let s = r.session(&["lab/*"], None, None);
    let ranked = r.face.retrieve(&s, &question("lab/indexed"), Bounds::default()).unwrap();
    assert!(ids(&ranked, "passage_id").contains(&"p0000".to_string()), "{:?}", ids(&ranked, "passage_id"));
    assert!(ranked.blocks["contextful.retrieval"].get("underfilled").is_none(), "{:?}", ranked.blocks);
    // Past 2048 nearer hidden rows the fourth round still sees none of the reader's: the
    // read answers and names the under-fill.
    let r = crowded_reads(2200);
    let s = r.session(&["lab/*"], None, None);
    let ranked = r.face.retrieve(&s, &question("lab/indexed"), Bounds::default()).unwrap();
    assert!(!ids(&ranked, "passage_id").contains(&"p0000".to_string()));
    assert_eq!(ranked.blocks["contextful.retrieval"]["underfilled"], json!(true), "{:?}", ranked.blocks);
}

fn battery(table: &str) -> RetrieveRequest {
    RetrieveRequest { query_embedding: Some(vec![0.0, 0.0, 1.0]), ..ask(table, "battery") }
}

/// The current snapshot directory of `table` and the directory of its one sidecar.
fn sidecar_dirs(r: &Reads, table: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (chain, _) = r.store.chain(table).unwrap();
    let dir = r.store.snapshot_dir(table, &chain[0].snapshot_id).unwrap();
    let path = chain[0].indexes[0].path().unwrap().to_string();
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

/// Lexical term statistics come from the widened candidate window, so an accelerated arm can order rows differently from the exact path.
// spec: read.rank.widened-window-statistics@3374cd12
#[cfg(feature = "fts")]
#[test]
fn sidecar_widening_changes_the_bm25_order_of_shared_rows() {
    let r = sidecar_reads_with_rows("", |i| match i {
        299 => ("battery".into(), json!([0.0, 0.0, 1.0])),
        298 => ("solar solar solar".into(), json!([0.0, 0.0, 1.0])),
        0..100 => ("solar".into(), json!([0.0, 0.0, 1.0])),
        _ => ("passage".into(), json!([1.0, 0.0, 0.0])),
    });
    let s = r.session(&["lab/*"], None, None);
    let request = |table| RetrieveRequest {
        query_embedding: Some(vec![0.0, 0.0, 1.0]),
        ..ask(table, "solar battery")
    };
    let exact = r.face.retrieve(&s, &request("lab/plain"), Bounds::default()).unwrap();
    let accelerated = r.face.retrieve(&s, &request("lab/indexed"), Bounds::default()).unwrap();
    assert_eq!(ids(&exact, "passage_id")[..2], ["p298", "p299"]);
    assert_eq!(ids(&accelerated, "passage_id")[..2], ["p299", "p298"]);
    assert_eq!(exact.blocks["contextful.retrieval"]["candidates_prefloor"], json!(200));
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

fn filtered(prefix: &str, query: &str, filter: Value) -> RetrieveRequest {
    RetrieveRequest { filter: Some(filter), min_score: Some(0), ..ask(prefix, query) }
}

fn tables(r: &Response) -> std::collections::BTreeSet<String> {
    column(r, "_table").iter().map(|t| t.as_str().unwrap().to_string()).collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

/// `filter` maps each caller-named column to a value, an equality, or a value list, a membership, and a row matches every condition. A table lacking a named column drops its arm, never emitting it unfiltered.
// spec: read.retrieve.unsatisfiable-arm-drops@b3c9aa26
#[test]
fn a_filter_binds_named_columns_and_a_table_lacking_one_drops_its_arm() {
    let r = Reads::new();
    let s = r.session(&["research/*"], Some(("research/notes", "acme")), None);
    let all = r.face.retrieve(&s, &RetrieveRequest { min_score: Some(0), ..ask("research/", "solar battery storage") }, Bounds::default()).unwrap();
    assert!(tables(&all).contains("research/contacts") && tables(&all).contains("research/notes"), "{:?}", all.rows);
    let member = r.face.retrieve(&s, &filtered("research/", "solar battery storage", json!({ "note_id": ["n1", "n3"] })), Bounds::default()).unwrap();
    assert_eq!(tables(&member).into_iter().collect::<Vec<_>>(), ["research/notes"]);
    assert_eq!(sorted(ids(&member, "note_id")), ["n1", "n3"]);
    let equal = r.face.retrieve(&s, &filtered("research/", "solar battery storage", json!({ "note_id": "n2", "tenant": "acme" })), Bounds::default()).unwrap();
    assert_eq!(ids(&equal, "note_id"), ["n2"]);
    let contact = r.face.retrieve(&s, &filtered("research/", "", json!({ "contact_id": "c2" })), Bounds::default()).unwrap();
    assert_eq!(ids(&contact, "contact_id"), ["c2"]);
    // A number matches a column holding that integral value.
    let s = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let priced = r.face.retrieve(&s, &filtered("research/", "feed", json!({ "Unit Price (USD)": 12 })), Bounds::default()).unwrap();
    assert_eq!(ids(&priced, "item_id"), ["v1"]);
    let mispriced = r.face.retrieve(&s, &filtered("research/", "feed", json!({ "Unit Price (USD)": [13, "x"] })), Bounds::default()).unwrap();
    assert!(mispriced.rows.is_empty(), "{:?}", mispriced.rows);
    let nowhere = r.face.retrieve(&s, &filtered("research/", "solar", json!({ "absent": "x" })), Bounds::default()).unwrap();
    assert!(nowhere.rows.is_empty(), "{:?}", nowhere.rows);

    // A row a sidecar recalls meets the filter on its re-join.
    let r = sidecar_reads("");
    let s = r.session(&["lab/*"], None, None);
    let request = RetrieveRequest { filter: Some(json!({ "passage_id": ["p001", "p299"] })), min_score: Some(0), ..battery("lab/indexed") };
    let recalled = r.face.retrieve(&s, &request, Bounds::default()).unwrap();
    assert_eq!(sorted(ids(&recalled, "passage_id")), ["p001", "p299"]);
    let request = RetrieveRequest { filter: Some(json!({ "passage_id": ["p000", "p001"] })), min_score: Some(0), ..battery("lab/indexed") };
    assert_eq!(ids(&r.face.retrieve(&s, &request, Bounds::default()).unwrap(), "passage_id")[0], "p000");
}

/// `kinds` lists artifact kind strings and joins the filter as one membership condition on the `kind` column, so a table without that column drops its arm by {{read.retrieve.unsatisfiable-arm-drops}}.
// spec: read.retrieve.kinds@4b91a583
#[test]
fn kinds_keep_listed_artifact_kinds_and_drop_tables_without_one() {
    let manifest = format!("{MANIFEST}\n[[pipeline.tables]]\nname = \"research/briefs\"\n");
    let mut r = Reads::with_manifest(&manifest);
    land_rows(
        &r.store,
        "research/briefs",
        "run-0001",
        json!([
            { "brief_id": "b1", "kind": "memo", "title": "Solar battery storage memo" },
            { "brief_id": "b2", "kind": "digest", "title": "Solar battery storage digest" },
            { "brief_id": "b3", "kind": "memo", "title": "Hiring memo" },
        ]),
    );
    r.face = Face::open(r.store.clone(), &manifest, pepper()).unwrap();
    let s = r.session(&["research/*"], Some(("research/notes", "acme")), None);
    let memos = r.face.retrieve(&s, &RetrieveRequest { kinds: Some(vec!["memo".into()]), ..ask("research/", "solar battery storage") }, Bounds::default()).unwrap();
    assert_eq!(ids(&memos, "brief_id"), ["b1"]);
    assert_eq!(column(&memos, "_kind"), [json!("memo")]);
    let both = RetrieveRequest {
        kinds: Some(vec!["memo".into(), "digest".into()]),
        filter: Some(json!({ "brief_id": ["b2", "b3"] })),
        min_score: Some(0),
        ..ask("research/", "solar battery storage")
    };
    let both = r.face.retrieve(&s, &both, Bounds::default()).unwrap();
    assert_eq!(tables(&both).into_iter().collect::<Vec<_>>(), ["research/briefs"]);
    assert_eq!(sorted(ids(&both, "brief_id")), ["b2", "b3"]);
}

/// The budget is checked over the whole filter ahead of building any arm; an oversized or malformed condition raises `FilterBudgetExceeded` for the whole read.
// spec: read.retrieve.filter-budget-refusal@89916c4f
#[test]
fn an_oversized_or_malformed_filter_refuses_the_whole_read() {
    let r = Reads::new();
    let s = r.session(&["research/*"], Some(("research/notes", "acme")), None);
    let wide = json!({ "note_id": (0..257).map(|i| format!("n{i}")).collect::<Vec<_>>() });
    refused_with(r.face.retrieve(&s, &filtered("research/", "solar", wide.clone()), Bounds::default()), "FilterBudgetExceeded");
    // One malformed condition refuses the read though another is satisfiable.
    let mixed = json!({ "note_id": "n1", "tenant": null });
    refused_with(r.face.retrieve(&s, &filtered("research/", "solar", mixed), Bounds::default()), "FilterBudgetExceeded");
    // The check precedes every arm: a prefix naming no table still refuses.
    refused_with(r.face.retrieve(&s, &filtered("nowhere/", "solar", wide), Bounds::default()), "FilterBudgetExceeded");
    let empty = RetrieveRequest { kinds: Some(Vec::new()), ..ask("research/", "solar") };
    refused_with(r.face.retrieve(&s, &empty, Bounds::default()), "FilterBudgetExceeded");
}

/// Regression: a ranked read over tables publishing no ceiling clamps its limit to the
/// face ceiling, and its candidate window follows the clamped limit.
#[test]
fn a_ranked_read_meets_the_face_ceiling() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let huge = r.face.retrieve(&s, &RetrieveRequest { limit: Some(u64::MAX), ..ask("research/vendor", "feed") }, Bounds::default()).unwrap();
    assert_eq!(huge.blocks["contextful.retrieval"]["window"], json!(8 * contextful_core::read::respond::FACE_ROW_CEILING));
}

/// `lab/hashed` declares its content-hash column; `lab/unhashed` holds the same rows and
/// declares none. `lab/dropped`, `lab/truncated` and `lab/digested` hold them too, under a
/// `drop`, a `truncate:1` and a `hash` mask on the content-hash column. `lab/revised` holds
/// one passage whose newest copy is retitled under an unchanged hash.
const DUPES: &str = r#"
[[pipeline.tables]]
name = "lab/hashed"
content_hash_column = "body_hash"

[[pipeline.tables]]
name = "lab/unhashed"

[[pipeline.tables]]
name = "lab/revised"
content_hash_column = "body_hash"
"#;

/// The masked tables' blocks, each with its column mask when `masked` holds.
fn masked_dupes(masked: bool) -> String {
    [("lab/dropped", "drop"), ("lab/truncated", "truncate:1"), ("lab/digested", "hash")]
        .iter()
        .map(|(table, strategy)| {
            let mask =
                if masked { format!("[pipeline.tables.policy.columns]\nbody_hash = {{ strategy = \"{strategy}\" }}\n") } else { String::new() };
            format!("\n[[pipeline.tables]]\nname = \"{table}\"\ncontent_hash_column = \"body_hash\"\n{mask}")
        })
        .collect()
}

/// Five runs a day apart, each landing one copy of one battery passage under the content
/// hash `h-a`, a passage under its own hash, and a passage with a null hash; `lab/revised`
/// takes one copy of passage `r` under `h-r` per run, the fifth retitled off the battery
/// query. Masks bind at query time alone.
fn dupe_reads() -> Reads {
    let landing = format!("{MANIFEST}{DUPES}{}", masked_dupes(false));
    let mut r = Reads::with_manifest(MANIFEST);
    for table in ["lab/hashed", "lab/unhashed", "lab/dropped", "lab/truncated", "lab/digested", "lab/revised"] {
        let decl = TableDecl::parse_pipeline(&landing).unwrap().into_iter().find(|d| d.name == table).unwrap();
        for run in 1..=5 {
            let rows = if table == "lab/revised" {
                let title = if run == 5 { "Grid maintenance notes" } else { "Battery storage cells" };
                json!([{ "passage_id": format!("r{run}"), "title": title, "body_hash": "h-r" }])
            } else {
                json!([
                    { "passage_id": format!("a{run}"), "title": "Battery storage cells", "body_hash": "h-a" },
                    { "passage_id": format!("b{run}"), "title": "Battery storage grids", "body_hash": format!("h-b{run}") },
                    { "passage_id": format!("c{run}"), "title": "Battery storage prices", "body_hash": null },
                ])
            };
            let ctx = RunContext {
                node: NodeId::parse("ingest-a").unwrap(),
                injection: Injection { run_id: format!("run-000{run}"), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
                committed_at: at(&format!("2030-01-1{run}T00:00:00Z")),
            };
            let rows = rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
            land(&r.store, &decl, &Batch { rows, types: HashMap::new() }, &ctx).unwrap();
        }
    }
    r.face = Face::open(r.store.clone(), &format!("{MANIFEST}{DUPES}{}", masked_dupes(true)), pepper()).unwrap();
    r
}

/// The share of returned rows repeating an earlier `(table, row key)` pair, the row key
/// read from the declared content-hash column.
fn duplicate_row_rate(ranked: &Response) -> f64 {
    let mut seen = std::collections::HashSet::new();
    let pairs: Vec<(Value, Value)> =
        column(ranked, "_table").into_iter().zip(column(ranked, "_row").into_iter().map(|r| r["body_hash"].clone())).collect();
    let repeats = pairs.iter().filter(|(_, key)| !key.is_null()).filter(|p| !seen.insert((*p).clone())).count();
    if pairs.is_empty() {
        0.0
    } else {
        repeats as f64 / pairs.len() as f64
    }
}

/// A ranked read keeps per `(table, row key)` only the newest ingestion, and no older copy when the newest misses the relevance floor. The row key is the declared content-hash column, else null, never a digest over projected values.
// spec: read.retrieve.row-key-dedup@77ce4ce8
#[test]
fn a_ranked_read_keeps_the_newest_row_per_content_hash() {
    let r = dupe_reads();
    let s = r.session(&["lab/*"], None, None);
    let ranked = r.face.retrieve(&s, &RetrieveRequest { limit: Some(50), ..ask("lab/hashed", "battery storage") }, Bounds::default()).unwrap();
    let mut kept = ids(&ranked, "passage_id");
    kept.sort();
    // One `h-a` copy survives, the fifth run's; every `h-b*` row and every null-key row stays.
    assert_eq!(kept, ["a5", "b1", "b2", "b3", "b4", "b5", "c1", "c2", "c3", "c4", "c5"]);
    let rate = duplicate_row_rate(&ranked);
    contextful_eval::record::emit("row-key-dedup", rate, ranked.rows.len() as u64, 0);
    assert_eq!(rate, 0.0);
    assert_eq!(ranked.blocks["contextful.retrieval"]["deduped"], json!(4));
    // Identical rows under no declared column stay whole: no digest stands in for the key.
    let unhashed = r.face.retrieve(&s, &RetrieveRequest { limit: Some(50), ..ask("lab/unhashed", "battery storage") }, Bounds::default()).unwrap();
    assert_eq!(unhashed.rows.len(), 15);
    assert_eq!(unhashed.blocks["contextful.retrieval"]["deduped"], json!(0));
    // `r5`, the newest `h-r` copy, misses the floor; no older copy stands in for it.
    let revised = r.face.retrieve(&s, &RetrieveRequest { limit: Some(50), ..ask("lab/revised", "battery storage") }, Bounds::default()).unwrap();
    assert_eq!(ids(&revised, "passage_id"), Vec::<String>::new());
    assert_eq!(revised.blocks["contextful.retrieval"]["candidates_prefloor"], json!(5));
    assert_eq!(revised.blocks["contextful.retrieval"]["deduped"], json!(4));
    let grid = r.face.retrieve(&s, &RetrieveRequest { limit: Some(50), ..ask("lab/revised", "grid maintenance") }, Bounds::default()).unwrap();
    assert_eq!(ids(&grid, "passage_id"), ["r5"]);
}

/// The row key is absent from the outer projection.
// spec: read.retrieve.row-key-stays-internal@0987d291
#[test]
fn the_row_key_stays_out_of_the_projection() {
    let r = dupe_reads();
    let s = r.session(&["lab/*"], None, None);
    let ranked = r.face.retrieve(&s, &ask("lab/hashed", "battery storage"), Bounds::default()).unwrap();
    assert_eq!(ranked.blocks["contextful.retrieval"]["deduped"], json!(4));
    // The deduplicating arm projects exactly what the same rows under no row key project.
    let plain = r.face.retrieve(&s, &ask("lab/unhashed", "battery storage"), Bounds::default()).unwrap();
    assert_eq!(plain.blocks["contextful.retrieval"]["deduped"], json!(0));
    assert_eq!(ranked.columns, plain.columns);
    let keys = |resp: &Response| -> std::collections::BTreeSet<Vec<String>> {
        column(resp, "_row").iter().map(|row| row.as_object().unwrap().keys().cloned().collect()).collect()
    };
    assert_eq!(keys(&ranked), keys(&plain));
    assert_eq!(keys(&ranked).into_iter().collect::<Vec<_>>(), [vec!["body_hash".to_string(), "passage_id".into(), "title".into()]]);
}

/// A content-hash column the session masks by `drop`, `truncate`, `bucket`, `range` or any combine keys every row null; one masked by `hash` or `tokenize` alone keys by its digest.
// spec: read.retrieve.row-key-under-mask@47dfc71e
#[test]
fn a_lossy_mask_on_the_row_key_keeps_every_row() {
    let r = dupe_reads();
    let s = r.session(&["lab/*"], None, None);
    let read = |table: &str| r.face.retrieve(&s, &RetrieveRequest { limit: Some(50), ..ask(table, "battery storage") }, Bounds::default()).unwrap();
    for table in ["lab/dropped", "lab/truncated"] {
        let masked = read(table);
        assert_eq!((masked.rows.len(), masked.blocks["contextful.retrieval"]["deduped"].clone()), (15, json!(0)), "{table}");
    }
    let digested = read("lab/digested");
    assert_eq!((digested.rows.len(), digested.blocks["contextful.retrieval"]["deduped"].clone()), (11, json!(4)));
    let mut kept = ids(&digested, "passage_id");
    kept.sort();
    assert_eq!(kept, ["a5", "b1", "b2", "b3", "b4", "b5", "c1", "c2", "c3", "c4", "c5"]);
}

/// The deduplicating window function runs under the same condition as the relevance floor, and a browse-shaped read skips it.
// spec: read.retrieve.dedup-is-gated@fd075b7a
#[test]
fn a_browse_shaped_read_skips_the_deduplicator() {
    let r = dupe_reads();
    let s = r.session(&["lab/*"], None, None);
    let browse = r.face.retrieve(&s, &RetrieveRequest { limit: Some(50), ..ask("lab/hashed", "") }, Bounds::default()).unwrap();
    assert_eq!(browse.blocks["contextful.retrieval"]["floor"], Value::Null);
    assert_eq!((browse.rows.len(), browse.blocks["contextful.retrieval"]["deduped"].clone()), (15, json!(0)));
    let ranked = r.face.retrieve(&s, &RetrieveRequest { limit: Some(50), ..ask("lab/hashed", "battery") }, Bounds::default()).unwrap();
    assert_eq!(ranked.blocks["contextful.retrieval"]["floor"], json!(1));
    assert_eq!((ranked.rows.len(), ranked.blocks["contextful.retrieval"]["deduped"].clone()), (11, json!(4)));
}

/// A claims table declaring `decay_half_life` scales each claim's fused score by its age
/// from `valid_from`; the same claims rank by relevance alone without it, and a table of
/// another shape declaring one refuses.
#[test]
fn a_declared_half_life_scales_a_claims_ranked_score() {
    use contextful_core::store::reconcile::ColumnType;
    let facts = "[[table]]\nname = \"research/insights\"\nshape = \"memory_facts\"\n{decay}columns = [\"claim_id\", \"subject\", \"predicate\", \"object\", \"scope\", \"tier\", \"confidence\", \"valid_from\", \"valid_to\", \"evidence\", \"superseded_by\", \"grant_id\", \"agent\"]\n";
    let ranked = |decay: &str| {
        let r = Reads::with_manifest(&format!("{MANIFEST}\n{}", facts.replace("{decay}", decay)));
        land_rows(&r.store, "research/sources", "src-1", json!([{ "source_id": "s1" }]));
        let evidence = r#"[{"table":"research/sources","run":"src-1","seq":0}]"#;
        let claim = |id: &str, subject: &str, from: &str| {
            json!({ "claim_id": id, "subject": subject, "predicate": "cfo", "object": id, "scope": null, "tier": "curated",
                "confidence": 1.0, "valid_from": from, "valid_to": null, "evidence": evidence, "superseded_by": null,
                "grant_id": "g", "agent": null })
        };
        let rows = json!([
            claim("old", "acme holdings", "2020-01-01T00:00:00Z"),
            claim("fresh", "acme", "2030-01-31T00:00:00Z"),
            claim("filler", "acme widgets corp", "2030-01-31T00:00:00Z"),
        ]);
        let rows = rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
        let types = [("valid_from", ColumnType::Timestamp), ("valid_to", ColumnType::Timestamp), ("confidence", ColumnType::Float64),
            ("scope", ColumnType::Utf8), ("superseded_by", ColumnType::Utf8), ("agent", ColumnType::Utf8)]
            .iter()
            .map(|(c, t)| (c.to_string(), t.clone()))
            .collect();
        let ctx = RunContext {
            node: NodeId::parse("ingest-a").unwrap(),
            injection: Injection { run_id: "memory-1".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
            committed_at: at("2030-01-10T00:00:00Z"),
        };
        let decl = TableDecl { primary_key: Some(vec!["claim_id".into()]), ..TableDecl::named("research/insights") };
        land(&r.store, &decl, &Batch { rows, types }, &ctx).unwrap();
        let s = r.session(&["research/*"], None, None);
        let answer = r.face.retrieve(&s, &ask("research/insights", "acme holdings"), Bounds::default()).unwrap();
        ids(&answer, "claim_id")
    };
    assert_eq!(ranked(""), ["old", "fresh", "filler"]);
    assert_eq!(ranked("decay_half_life = \"365d\"\n"), ["fresh", "filler", "old"]);

    let entities = "[[table]]\nname = \"research/people\"\nshape = \"memory_entities\"\ndecay_half_life = \"365d\"\ncolumns = [\"entity_id\", \"kind\", \"name\", \"aliases\"]\n";
    let dir = tempfile::tempdir().unwrap();
    let refused = Face::open(Store::open(dir.path(), "research").unwrap(), &format!("{MANIFEST}\n{entities}"), Pepper::resolve(|_| None));
    assert!(refused.err().is_some_and(|e| e.to_string().contains("decay_half_life")));
}
