//! `read.retrieve` and `read.rank` over a full-text sidecar: the probe across the whole
//! snapshot, CJK phrase matching, the fallback to the window, and the opened-sidecar cache.

use super::*;
use contextful_context::fold::fold;
use contextful_context::fulltext::SidecarCache;
use contextful_context::read::RetrieveRequest;
use contextful_context::vector::Fallback;
use contextful_core::read::rank::LEXICAL_INDEX_CACHE_ENTRIES;
use contextful_core::store::bound_time::Bounds;

/// Two tables of one row set, `lab/plain` and `lab/text`, the second declaring a CJK
/// full-text sidecar over `body`; `extra` extends the indexed table's block.
const TEXT: &str = r#"
[[pipeline.tables]]
name = "lab/plain"
primary_key = ["passage_id"]

[[pipeline.tables]]
name = "lab/text"
primary_key = ["passage_id"]

[[pipeline.tables.indexes]]
kind = "fulltext"
column = "body"
tokenizer = "cjk"
"#;

fn ask(prefix: &str, query: &str) -> RetrieveRequest {
    RetrieveRequest::new(prefix, query, at("2030-02-01T00:00:00Z"))
}

fn ids(r: &Response) -> Vec<String> {
    column(r, "_row").iter().map(|row| row["passage_id"].as_str().unwrap().to_string()).collect()
}

/// Land `bodies` into both tables as one run, row `i` keyed `p<i>`, and fold each. The
/// first rows land first, so a recency window reads the last ones.
fn land_both(r: &Reads, manifest: &str, bodies: &[String], run: &str, fold_at: &str) {
    let rows: Vec<serde_json::Map<String, Value>> = bodies
        .iter()
        .enumerate()
        .map(|(i, b)| json!({"passage_id": format!("p{i:05}"), "body": b}).as_object().unwrap().clone())
        .collect();
    for table in ["lab/plain", "lab/text"] {
        let decl = TableDecl::parse_pipeline(manifest).unwrap().into_iter().find(|d| d.name == table).unwrap();
        let ctx = RunContext {
            node: NodeId::parse("ingest-a").unwrap(),
            injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, authored_agent: None, taint: None },
            committed_at: at("2030-01-10T00:00:00Z"),
        };
        land(&r.store, &decl, &Batch { rows: rows.clone(), types: HashMap::new() }, &ctx).unwrap();
        fold(&r.store, &decl, at(fold_at)).unwrap();
    }
}

/// 300 passages per table. The five oldest sit outside a limit-10 recency window: `p00000`
/// holds a Japanese sentence, `p00001` a near miss sharing its first character, `p00002` a
/// plural, `p00003` both bigrams of `東京駅` apart, and `p00004` the same bigrams adjacent.
fn text_reads(extra: &str) -> Reads {
    let manifest = format!("{MANIFEST}{TEXT}{extra}");
    let mut r = Reads::with_manifest(&format!("{MANIFEST}{TEXT}"));
    let bodies: Vec<String> = (0..300)
        .map(|i| match i {
            0 => "メニューの設定画面を開く".to_string(),
            1 => "設計図を確認する".to_string(),
            2 => "Overdue invoices from March".to_string(),
            3 => "京駅と東京".to_string(),
            4 => "東京駅で会う".to_string(),
            _ => format!("routine passage {i}"),
        })
        .collect();
    land_both(&r, &manifest, &bodies, "run-0001", "2030-01-11T00:00:00Z");
    r.face = Face::open(r.store.clone(), &manifest, pepper()).unwrap();
    r
}

#[test]
fn describe_reports_the_declared_fulltext_index() {
    let r = text_reads("");
    let session = r.session(&["lab/*"], None, None);
    let described = r.face.describe(&session, Some("lab/text"), Bounds::default()).unwrap();
    assert_eq!(described["indexes"], json!([{"kind": "fulltext", "column": "body", "tokenizer": "cjk"}]));
}

/// Full-text probes rank the entire snapshot by BM25 with one should-clause per content token. Tokens split by the
/// sidecar tokenizer match resulting terms at consecutive positions. ASCII word tokens match plurals under
/// {{read.retrieve.script-split-matching}}.
// spec: read.retrieve.fulltext-probe@9656cf0e
#[cfg(feature = "fts")]
#[test]
fn a_probe_matches_unspaced_phrases_and_plurals_across_the_snapshot() {
    let r = text_reads("");
    let s = r.session(&["lab/*"], None, None);
    for (query, expected) in [("設定画面", "p00000"), ("画面", "p00000"), ("設計", "p00001"), ("invoice", "p00002")] {
        // The recency window alone never reaches the three oldest rows.
        assert!(ids(&r.face.retrieve(&s, &ask("lab/plain", query), Bounds::default()).unwrap()).is_empty(), "{query}");
        let found = r.face.retrieve(&s, &ask("lab/text", query), Bounds::default()).unwrap();
        assert_eq!(ids(&found), [expected], "{query}");
        assert_eq!(column(&found, "_score"), [json!(1)], "{query}");
    }
    // `設定` and `設計` share a character and no bigram: neither query finds the other row.
    let (_, setting) = r.face.fulltext_candidates(&s, "lab/text", &["設定".to_string()], 10).unwrap();
    assert_eq!(setting, ["p00000"]);
    let (id_column, design) = r.face.fulltext_candidates(&s, "lab/text", &["設計".to_string()], 10).unwrap();
    assert_eq!((id_column.as_str(), design), ("passage_id", vec!["p00001".to_string()]));
    // A phrase needs its bigrams adjacent: `設定` and `画面` both occur, `定を` does not.
    assert!(r.face.fulltext_candidates(&s, "lab/text", &["設定を".to_string()], 10).unwrap().1.is_empty());
    // `p00003` holds `東京` and `京駅` apart and `p00004` holds them adjacent: only the second matches `東京駅`.
    let (_, station) = r.face.fulltext_candidates(&s, "lab/text", &["東京駅".to_string()], 10).unwrap();
    assert_eq!(station, ["p00004"]);
    // One should-clause per token: a row matching either token is a candidate.
    let (_, either) = r.face.fulltext_candidates(&s, "lab/text", &["画面".to_string(), "invoice".to_string()], 10).unwrap();
    assert_eq!(either.len(), 2, "{either:?}");
}

/// If declared `retrieve.text` leaves no column, retrieval skips that table's full-text probe. For queries containing
/// a content token, only positive vector scores admit its rows.
// spec: read.retrieve.empty-scored-text@251216a2
#[test]
fn an_empty_declared_text_list_adds_no_lexical_candidate() {
    let indexed = "name = \"lab/text\"\nprimary_key = [\"passage_id\"]\n";
    let manifest = format!("{MANIFEST}{}", TEXT.replace(indexed, &format!("{indexed}retrieve = {{ text = [] }}\n")));
    let mut r = Reads::with_manifest(&manifest);
    let bodies: Vec<String> = (0..300).map(|i| if i == 2 { "Overdue invoices from March".to_string() } else { format!("routine passage {i}") }).collect();
    land_both(&r, &manifest, &bodies, "run-0001", "2030-01-11T00:00:00Z");
    r.face = Face::open(r.store.clone(), &manifest, pepper()).unwrap();
    let s = r.session(&["lab/*"], None, None);
    // The sidecar finds the row, so only the declaration keeps the probe from running.
    assert_eq!(r.face.fulltext_candidates(&s, "lab/text", &["invoice".to_string()], 10).unwrap().1, ["p00002"]);
    for query in ["invoice", "routine passage"] {
        let ranked = r.face.retrieve(&s, &ask("lab/text", query), Bounds::default()).unwrap();
        assert_eq!(ids(&ranked), Vec::<String>::new(), "{query}");
        assert_eq!(ranked.blocks["contextful.retrieval"]["candidates_prefloor"], json!(0), "{query}");
    }
    // A query with no content token applies no floor, and the table still browses.
    assert_eq!(ids(&r.face.retrieve(&s, &RetrieveRequest { limit: Some(3), ..ask("lab/text", "") }, Bounds::default()).unwrap()).len(), 3);
}

/// A term on the oldest of 10,001 rows returns for limit 10 (ledger `lexical-deep-recall`).
#[cfg(feature = "fts")]
#[test]
fn a_term_on_one_row_behind_10000_newer_rows_returns_for_limit_10() {
    let manifest = format!("{MANIFEST}{TEXT}");
    let mut r = Reads::with_manifest(&manifest);
    let bodies: Vec<String> =
        (0..10_001).map(|i| if i == 0 { "the zephyrine ledger".to_string() } else { format!("routine passage {i}") }).collect();
    land_both(&r, &manifest, &bodies, "run-0001", "2030-01-11T00:00:00Z");
    r.face = Face::open(r.store.clone(), &manifest, pepper()).unwrap();
    let s = r.session(&["lab/*"], None, None);
    let request = |table: &str| RetrieveRequest { limit: Some(10), ..ask(table, "zephyrine") };
    assert!(ids(&r.face.retrieve(&s, &request("lab/plain"), Bounds::default()).unwrap()).is_empty());
    let found = ids(&r.face.retrieve(&s, &request("lab/text"), Bounds::default()).unwrap());
    let hit = found.first().map(String::as_str) == Some("p00000");
    contextful_eval::record::emit("lexical-deep-recall", if hit { 1.0 } else { 0.0 }, 1, 0);
    assert!(hit, "{found:?}");
}

/// Every failed full-text precondition leaves the window-bound ranking unchanged.
#[test]
fn a_read_without_its_full_text_sidecar_ranks_the_window_unchanged() {
    let r = text_reads("");
    let s = r.session(&["lab/*", "research/*"], None, None);
    let tokens = ["画面".to_string()];
    assert_eq!(r.face.fulltext_candidates(&s, "research/notes", &tokens, 10).err(), Some(Fallback::NoSnapshot));
    assert_eq!(r.face.fulltext_candidates(&s, "lab/plain", &tokens, 10).err(), Some(Fallback::NoSidecar));
    let plain = |r: &Reads, s: &Session, q: &str| ids(&r.face.retrieve(s, &ask("lab/plain", q), Bounds::default()).unwrap());
    let indexed = |r: &Reads, s: &Session, q: &str| ids(&r.face.retrieve(s, &ask("lab/text", q), Bounds::default()).unwrap());

    // A masked or classed text column, a masked identifier, and a zone withholding the text.
    for policy in [
        "[pipeline.tables.policy.columns]\nbody = { strategy = \"drop\" }\n",
        "[pipeline.tables.policy.columns]\npassage_id = { strategy = \"hash\" }\n",
        "[pipeline.tables.policy.zone]\nallow = [\"on-prem:*\", \"public-cloud:*\"]\n[pipeline.tables.policy.columns]\nbody = { class = \"phi\" }\n",
    ] {
        let m = text_reads(policy);
        for zone in [None, Some("public-cloud:us-east-1")] {
            let ms = m.session(&["lab/*"], None, zone);
            assert_eq!(m.face.fulltext_candidates(&ms, "lab/text", &tokens, 10).err(), Some(Fallback::Withheld), "{policy} {zone:?}");
            // The arm adds nothing: the candidates are at most the 200-row window.
            let read = m.face.retrieve(&ms, &ask("lab/text", "画面"), Bounds::default()).unwrap();
            assert!(read.blocks["contextful.retrieval"]["candidates_prefloor"].as_u64().unwrap() <= 200, "{policy} {zone:?}");
        }
    }

    // A sidecar manifest disagreeing with the snapshot's entry, then unreadable postings.
    let fresh = text_reads("");
    let fs_ = fresh.session(&["lab/*"], None, None);
    let (chain, _) = fresh.store.chain("lab/text").unwrap();
    let dir = fresh.store.snapshot_dir("lab/text", &chain[0].snapshot_id).unwrap().join(chain[0].indexes[0].path().unwrap());
    let own = dir.join("_manifest.json");
    let original = std::fs::read_to_string(&own).unwrap();
    std::fs::write(&own, original.replace("\"tokenizer\": \"cjk\"", "\"tokenizer\": \"unicode\"")).unwrap();
    assert_eq!(fresh.face.fulltext_candidates(&fs_, "lab/text", &tokens, 10).err(), Some(Fallback::ManifestMismatch));
    assert_eq!(indexed(&fresh, &fs_, "画面"), plain(&fresh, &fs_, "画面"));
    std::fs::write(&own, &original).unwrap();
    let postings = dir.join("postings.bin");
    let bytes = std::fs::read(&postings).unwrap();
    std::fs::write(&postings, &bytes[..bytes.len() / 2]).unwrap();
    assert_eq!(fresh.face.fulltext_candidates(&fs_, "lab/text", &tokens, 10).err(), Some(Fallback::Unreadable));
    std::fs::remove_file(&postings).unwrap();
    assert_eq!(fresh.face.fulltext_candidates(&fs_, "lab/text", &tokens, 10).err(), Some(Fallback::Unreadable));
    assert_eq!(indexed(&fresh, &fs_, "画面"), plain(&fresh, &fs_, "画面"));
    std::fs::write(&postings, &bytes).unwrap();
    assert_eq!(fresh.face.fulltext_candidates(&fs_, "lab/text", &tokens, 10).unwrap().1, ["p00000"]);
}

/// The opened full-text sidecar cache holds at most 64 entries in FIFO order, keyed by table, snapshot, path and key-
/// version fingerprint. Repeated reads reuse sidecars; a different snapshot requires opening another.
// spec: read.rank.lexical-index-cache@86227973
#[cfg(feature = "fts")]
#[test]
fn an_opened_sidecar_serves_repeated_reads_until_a_new_snapshot() {
    let r = text_reads("");
    let s = r.session(&["lab/*"], None, None);
    assert_eq!(r.face.fulltext_cache().counts(), (0, 0));
    for query in ["画面", "invoice", "設定画面"] {
        r.face.retrieve(&s, &ask("lab/text", query), Bounds::default()).unwrap();
    }
    assert_eq!(r.face.fulltext_cache().counts(), (2, 1));
    assert_eq!(r.face.fulltext_cache().len(), 1);
    // A new snapshot opens its own sidecar, which sees the row it adds.
    let manifest = format!("{MANIFEST}{TEXT}");
    let bodies: Vec<String> = (0..301)
        .map(|i| match i {
            0 => "メニューの設定画面を開く".to_string(),
            300 => "新しい画面".to_string(),
            _ => format!("routine passage {i}"),
        })
        .collect();
    land_both(&r, &manifest, &bodies, "run-0002", "2030-01-12T00:00:00Z");
    let (_, found) = r.face.fulltext_candidates(&s, "lab/text", &["画面".to_string()], 10).unwrap();
    assert_eq!(r.face.fulltext_cache().counts(), (2, 2));
    assert!(found.contains(&"p00300".to_string()) && found.contains(&"p00000".to_string()), "{found:?}");

    // Past 64 entries the oldest leaves first; a failed open caches nothing.
    assert_eq!(LEXICAL_INDEX_CACHE_ENTRIES, 64);
    let cache: SidecarCache<usize> = SidecarCache::new(LEXICAL_INDEX_CACHE_ENTRIES);
    for i in 0..=LEXICAL_INDEX_CACHE_ENTRIES {
        cache.get_or_open::<()>(&format!("k{i}"), || Ok(i)).unwrap();
    }
    assert_eq!((cache.len(), cache.counts()), (64, (0, 65)));
    assert_eq!(*cache.get_or_open::<()>("k64", || Ok(0)).unwrap(), 64);
    assert_eq!(*cache.get_or_open::<()>("k0", || Ok(1000)).unwrap(), 1000);
    assert_eq!(cache.get_or_open("absent", || Err("unreadable")).err(), Some("unreadable"));
    assert_eq!((cache.len(), cache.counts()), (64, (1, 67)));
}

/// Under a horizon, sidecar probes return only entries available at or before that horizon. Full-text scoring derives
/// document count, average length and document frequency solely from those entries.
// spec: read.horizon.rank-statistics@d8bf6998
#[test]
fn a_ranking_under_a_horizon_scores_only_rows_available_by_it() {
    use contextful_context::fulltext::FulltextSidecar;
    use contextful_core::grant::Grant;
    use contextful_core::store::bound_time::Bound;
    use contextful_core::store::reconcile::ColumnType;
    let manifest = format!("{MANIFEST}{DATED_TEXT}");
    let decl = TableDecl::parse_pipeline(&manifest).unwrap().into_iter().find(|d| d.name == "lab/news").unwrap();
    let stories = json!([
        { "story_id": "s-2013", "digest": "a", "body": "acme margin report", "published_at": "2013-03-01T00:00:00Z" },
        { "story_id": "s-2014", "digest": "b", "body": "acme widens margin", "published_at": "2014-11-01T00:00:00Z" },
        { "story_id": "s-2015", "digest": "c", "body": "margin margin margin widens again", "published_at": "2015-06-01T00:00:00Z" },
        { "story_id": "s-2016", "digest": "d", "body": "margin call", "published_at": "2016-02-01T00:00:00Z" },
    ]);
    // A store holding the first `upto` stories, landed as one run and folded.
    let store_of = |upto: usize| {
        let mut r = Reads::with_manifest(&manifest);
        let ctx = RunContext {
            node: NodeId::parse("ingest-a").unwrap(),
            injection: Injection { run_id: "run-0001".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, authored_agent: None, taint: None },
            committed_at: at("2030-01-10T00:00:00Z"),
        };
        let rows = stories.as_array().unwrap()[..upto].iter().map(|r| r.as_object().unwrap().clone()).collect();
        land(&r.store, &decl, &Batch { rows, types: [("published_at".to_string(), ColumnType::Timestamp)].into_iter().collect() }, &ctx).unwrap();
        fold(&r.store, &decl, at("2030-01-11T00:00:00Z")).unwrap();
        r.face = Face::open(r.store.clone(), &manifest, pepper()).unwrap();
        r
    };
    let (whole, bounded) = (store_of(4), store_of(2));
    let horizon = "2015-01-01T00:00:00Z";

    // The sidecar's probe under the horizon scores the two stories published by then as a
    // sidecar built over those two alone scores them.
    let probe = |r: &Reads, bound: Option<Bound>| -> Vec<(String, f64)> {
        let snapshot = r.store.chain("lab/news").unwrap().0.remove(0);
        let dir = r.store.snapshot_dir("lab/news", &snapshot.snapshot_id).unwrap();
        let entry = snapshot.indexes.iter().find(|e| e.kind_name() == Some("fulltext")).unwrap();
        let sidecar = FulltextSidecar::open(&dir, "lab/news", entry, &r.store.sealing()).unwrap();
        let tokens = ["widens".to_string(), "margin".to_string()];
        sidecar.probe_within(&tokens, 10, bound).unwrap().candidates.into_iter().map(|c| (c.id, c.score)).collect()
    };
    let within = probe(&whole, Some(Bound::parse(horizon).unwrap()));
    assert_eq!(within.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["s-2014", "s-2013"]);
    assert_eq!(within, probe(&bounded, None));
    // Unbounded, the later stories' frequencies and lengths move every score.
    let unbounded = probe(&whole, None);
    assert_eq!(unbounded.len(), 4);
    assert!(unbounded.iter().all(|(id, score)| within.iter().all(|(w, s)| w != id || s != score)), "{unbounded:?}");

    // A ranked read under the horizon answers through the sidecar and returns the bounded
    // store's ranking, identifiers and scores alike.
    let ranked = |r: &Reads, horizon: &str| -> Vec<(String, Value)> {
        let bounds = Bounds { available_as_of: Some(Bound::parse(horizon).unwrap()), ..Bounds::default() };
        let grant = Grant { horizon: Some(at(horizon)), ..read(&["lab/*"], None) };
        let session = r.face.session(&r.authority(loop_subject("agent://research-loop"), vec![grant]), &Request::default(), bounds).unwrap();
        let response = r.face.retrieve(&session, &ask("lab/news", "widens margin"), bounds).unwrap();
        column(&response, "_row").iter().map(|row| row["story_id"].as_str().unwrap().to_string()).zip(column(&response, "_score")).collect()
    };
    let reference = ranked(&bounded, horizon);
    assert_eq!(reference.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["s-2014", "s-2013"]);
    assert_eq!(ranked(&whole, horizon), reference);
    // A later horizon admits the stories published by it.
    assert_eq!(ids_of_ranked(&ranked(&whole, "2015-12-31T00:00:00Z")), ["s-2013", "s-2014", "s-2015"]);
    // Without a horizon every story ranks.
    let session = whole.face.session(&whole.authority(loop_subject("agent://research-loop"), vec![read(&["lab/*"], None)]), &Request::default(), Bounds::default()).unwrap();
    assert_eq!(ids_of(&whole.face.retrieve(&session, &ask("lab/news", "widens margin"), Bounds::default()).unwrap(), "story_id").len(), 4);
}

fn ids_of_ranked(ranking: &[(String, Value)]) -> Vec<String> {
    let mut ids: Vec<String> = ranking.iter().map(|(id, _)| id.clone()).collect();
    ids.sort();
    ids
}

fn ids_of(r: &Response, key: &str) -> Vec<String> {
    let mut ids: Vec<String> = column(r, "_row").iter().map(|row| row[key].as_str().unwrap().to_string()).collect();
    ids.sort();
    ids
}

/// A pass builds a declared sidecar the current snapshot lacks, or records with other `id_column` or builder
/// parameters, over those rows, publishing a new snapshot even without a landed run.
// spec: store.index.declared-unbuilt@d5b87055
#[test]
fn a_pass_builds_a_sidecar_declared_over_rows_already_landed() {
    use contextful_core::store::fold::FoldOutcome;
    let bare = format!("{MANIFEST}{}", TEXT.split("[[pipeline.tables.indexes]]").next().unwrap());
    let decl_of = |manifest: &str| TableDecl::parse_pipeline(manifest).unwrap().into_iter().find(|d| d.name == "lab/text").unwrap();
    let r = Reads::with_manifest(&bare);
    let rows: Vec<serde_json::Map<String, Value>> = [("p0", "d0", "Overdue invoices from March"), ("p1", "d1", "routine passage")]
        .iter()
        .map(|(id, digest, body)| json!({"passage_id": id, "digest": digest, "body": body}).as_object().unwrap().clone())
        .collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-0001".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, authored_agent: None, taint: None },
        committed_at: at("2030-01-10T00:00:00Z"),
    };
    land(&r.store, &decl_of(&bare), &Batch { rows, types: HashMap::new() }, &ctx).unwrap();
    assert!(matches!(fold(&r.store, &decl_of(&bare), at("2030-01-11T00:00:00Z")).unwrap(), FoldOutcome::Folded { runs: 1, .. }));
    let s = r.session(&["lab/*"], None, None);
    let tokens = ["invoice".to_string()];
    assert_eq!(r.face.fulltext_candidates(&s, "lab/text", &tokens, 10).err(), Some(Fallback::NoSidecar));
    // With no index change and nothing landed, a pass publishes nothing.
    assert_eq!(fold(&r.store, &decl_of(&bare), at("2030-01-11T01:00:00Z")).unwrap(), FoldOutcome::NothingLanded);

    // A newly declared sidecar, the same sidecar under another `id_column`, and another tokenizer.
    let steps = [
        (format!("{MANIFEST}{TEXT}"), "passage_id", "p0", "indexes/fts-body-cjk"),
        (format!("{MANIFEST}{TEXT}id_column = \"digest\"\n"), "digest", "d0", "indexes/fts-body-cjk"),
        (format!("{MANIFEST}{}", TEXT.replace("\"cjk\"", "\"unicode\"")), "passage_id", "p0", "indexes/fts-body-unicode"),
    ];
    for (i, (manifest, id_column, id, path)) in steps.iter().enumerate() {
        let decl = decl_of(manifest);
        let before = r.store.chain("lab/text").unwrap().0[0].snapshot_id.clone();
        let outcome = fold(&r.store, &decl, at(&format!("2030-01-12T0{i}:00:00Z"))).unwrap();
        assert!(matches!(outcome, FoldOutcome::Folded { runs: 0, rows: 2, .. }), "{manifest}: {outcome:?}");
        let (chain, _) = r.store.chain("lab/text").unwrap();
        assert_eq!(chain[0].parent.as_ref(), Some(&before));
        let built: Vec<&str> = chain[0].indexes.iter().filter_map(|e| e.path()).collect();
        assert_eq!(built, [*path], "{manifest}");
        // The lexical read probes the sidecar the pass built.
        let (column, found) = r.face.fulltext_candidates(&s, "lab/text", &tokens, 10).unwrap();
        assert_eq!((column.as_str(), found), (*id_column, vec![id.to_string()]), "{manifest}");
        // The sidecar now matches its declaration: the next pass publishes nothing.
        assert_eq!(fold(&r.store, &decl, at(&format!("2030-01-12T0{i}:30:00Z"))).unwrap(), FoldOutcome::NothingLanded, "{manifest}");
    }
}
