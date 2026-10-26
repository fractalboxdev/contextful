//! Point-in-time reads over the tool server: horizon projections, snapshot corpora and
//! adjustment factors.

use crate::mcp::{call, current, fixture_bounded, put, Fixture};
use contextful_agent::mcp::Server;
use contextful_context::Store;
use contextful_core::ports::FixedClock;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::time::Instant;
use serde_json::{json, Value};

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

/// The clock reads after every instant the fixtures land, so no bound sits in the future.
static CLOCK: std::sync::OnceLock<FixedClock> = std::sync::OnceLock::new();

fn server(f: &Fixture) -> (Server<'_>, ()) {
    let clock = CLOCK.get_or_init(|| FixedClock(at("2031-01-01T00:00:00Z")));
    (Server::new(&f.face, f.authority.clone(), &current, clock, &f.audit).unwrap(), ())
}

fn ok(answer: &Value) -> &Value {
    assert!(answer["result"].get("isError").is_none(), "{answer}");
    &answer["result"]["structuredContent"]
}

fn refused(answer: &Value) -> String {
    assert_eq!(answer["result"]["isError"], json!(true), "{answer}");
    answer["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

fn columns(answer: &Value) -> Vec<String> {
    ok(answer)["columns"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect()
}

fn described(answer: &Value) -> Vec<String> {
    ok(answer)["columns"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap().to_string()).collect()
}

fn decls(manifest: &str) -> impl Fn(&str) -> TableDecl {
    let decls = TableDecl::parse_pipeline(manifest).unwrap();
    move |name: &str| decls.iter().find(|d| d.name == name).unwrap().clone()
}

const EARNINGS: &str = r#"[[pipeline.tables]]
name = "research/earnings"
primary_key = ["filing_id"]
content_hash_column = "digest"

[pipeline.tables.available_time]
column = "accepted_at"
"#;

/// Two quarters' earnings backfilled on 1 September; the second carries a `guidance` field the
/// first never did.
fn earnings(horizon: Option<&str>) -> Fixture {
    let decl = decls(EARNINGS);
    fixture_bounded(EARNINGS, horizon, |store| {
        let ts = [("accepted_at", ColumnType::Timestamp)];
        let q1 = json!([{ "filing_id": "acme-q1", "ticker": "ACME", "eps": "1.23", "digest": "q1", "accepted_at": "2030-05-02T16:05:00Z" }]);
        put(store, &decl("research/earnings"), "run-1", "2030-09-01T00:00:00Z", q1, &ts);
        let q2 = json!([{ "filing_id": "acme-q2", "ticker": "ACME", "eps": "1.31", "guidance": "raised", "digest": "q2", "accepted_at": "2030-08-01T16:05:00Z" }]);
        put(store, &decl("research/earnings"), "run-2", "2030-09-02T00:00:00Z", q2, &ts);
    })
}

/// Under a horizon, responses omit `_ingested_at` and `_commit_seq` but retain citation fields `_run_id` and
/// `_row_seq`. `context.describe` computes row count, size, lexicon and example values from the horizon-bounded
/// relation.
// spec: read.horizon.engine-stamps@586a6493
#[test]
fn a_horizon_read_withholds_load_stamps_and_keeps_citation_stamps() {
    let sql = r#"SELECT * FROM "research/earnings""#;
    let open = earnings(None);
    let (s, _) = server(&open);
    let all = columns(&call(&s, "context.query", json!({ "sql": sql })));
    assert!(all.iter().any(|c| c == "_ingested_at") && all.iter().any(|c| c == "_commit_seq"), "{all:?}");

    let bounded = earnings(Some("2030-06-15T00:00:00Z"));
    let (s, _) = server(&bounded);
    let answer = call(&s, "context.query", json!({ "sql": sql }));
    let seen = columns(&answer);
    assert!(seen.iter().any(|c| c == "_run_id") && seen.iter().any(|c| c == "_row_seq"), "{seen:?}");
    assert!(!seen.iter().any(|c| c == "_ingested_at" || c == "_commit_seq"), "{seen:?}");
    assert_eq!(ok(&answer)["rows"].as_array().unwrap().len(), 1, "{answer}");
    let text = refused(&call(&s, "context.query", json!({ "sql": r#"SELECT "_ingested_at" FROM "research/earnings""# })));
    assert!(text.contains("_ingested_at"), "{text}");

    // The citation stamps still name the row.
    let reference = call(&s, "context.reference", json!({ "table": "research/earnings", "run": "run-1", "seq": 0 }));
    assert_eq!(ok(&reference)["rows"], json!([[true, "available"]]), "{reference}");

    // The description counts the bounded rows and lists no load stamp.
    let description = call(&s, "context.describe", json!({ "table": "research/earnings" }));
    assert_eq!(ok(&description)["row_count"], json!("1"), "{description}");
    let listed = described(&description);
    assert!(listed.iter().any(|c| c == "_run_id"), "{listed:?}");
    assert!(!listed.iter().any(|c| c == "_ingested_at" || c == "_commit_seq"), "{listed:?}");

    // Ranking orders without the load stamp.
    let ranked = call(&s, "corpus.retrieve", json!({ "prefix": "research/earnings", "query": "acme" }));
    assert_eq!(ok(&ranked)["rows"].as_array().unwrap().len(), 1, "{ranked}");
    let ranked_columns = columns(&ranked);
    assert!(ranked_columns.iter().any(|c| c == "_run_id") && !ranked_columns.iter().any(|c| c == "_ingested_at"), "{ranked_columns:?}");
}

/// A column's first merge sets `schema.json`'s `first_landed_at` once to the batch's earliest availability instant,
/// otherwise commit instant. Columns with `first_landed_at` after the horizon are absent from registered schemas and
/// describe payloads.
// spec: read.horizon.late-columns@b9ea72e3
#[test]
fn a_column_first_available_after_the_horizon_is_absent() {
    let open = earnings(None);
    let schema = open.face.store().schema("research/earnings").unwrap();
    // Both runs commit in September; each field dates from its batch's earliest availability.
    assert_eq!(schema.first_landed["eps"], at("2030-05-02T16:05:00Z"));
    assert_eq!(schema.first_landed["guidance"], at("2030-08-01T16:05:00Z"));
    let (s, _) = server(&open);
    assert!(described(&call(&s, "context.describe", json!({ "table": "research/earnings" }))).iter().any(|c| c == "guidance"));

    let bounded = earnings(Some("2030-06-15T00:00:00Z"));
    let (s, _) = server(&bounded);
    let listed = described(&call(&s, "context.describe", json!({ "table": "research/earnings" })));
    assert!(listed.iter().any(|c| c == "eps") && !listed.iter().any(|c| c == "guidance"), "{listed:?}");
    let eps = call(&s, "context.query", json!({ "sql": r#"SELECT filing_id, eps FROM "research/earnings""# }));
    assert_eq!(ok(&eps)["rows"], json!([["acme-q1", "1.23"]]), "{eps}");
    let text = refused(&call(&s, "context.query", json!({ "sql": r#"SELECT guidance FROM "research/earnings""# })));
    assert!(text.contains("guidance"), "{text}");

    // A later horizon admits the field.
    let later = earnings(Some("2030-08-02T00:00:00Z"));
    let (s, _) = server(&later);
    assert!(described(&call(&s, "context.describe", json!({ "table": "research/earnings" }))).iter().any(|c| c == "guidance"));
}

const PAGES: &str = r#"[[pipeline.tables]]
name = "research/pages"
primary_key = ["url"]
content_hash_column = "digest"

[pipeline.tables.available_time]
column = "published_at"

[pipeline.tables.snapshot]
url = "url"
captured_at = "captured_at"
digest = "digest"
source = "source"
leak_score = "leak_score"
"#;

const PAGE_TYPES: [(&str, ColumnType); 2] = [("published_at", ColumnType::Timestamp), ("captured_at", ColumnType::Timestamp)];

fn page(url: &str, digest: &str, published: &str, captured: &str, leak: Option<i64>, body: &str) -> Value {
    json!({ "url": url, "digest": digest, "published_at": published, "captured_at": captured, "source": "crawl", "leak_score": leak, "body": body })
}

/// Two captures of one corpus, landed a month apart; `ids` receives each capture's snapshot id.
fn captures(horizon: Option<&str>, ids: &mut Vec<String>) -> Fixture {
    let decl = decls(PAGES);
    fixture_bounded(PAGES, horizon, |store: &Store| {
        let pages = decl("research/pages");
        let first = json!([
            page("https://a.example/rates", "a1", "2014-03-02T00:00:00Z", "2014-03-03T00:00:00Z", None, "rates held steady"),
            page("https://b.example/jobs", "b1", "2014-03-10T00:00:00Z", "2014-03-11T00:00:00Z", None, "jobs report steady"),
        ]);
        put(store, &pages, "capture-1", "2014-03-12T00:00:00Z", first, &PAGE_TYPES);
        ids.push(contextful_context::build::manifests(store, "research/pages").unwrap().last().unwrap().snapshot_id.to_string());
        let second = json!([
            page("https://a.example/rates", "a2", "2014-03-02T00:00:00Z", "2019-06-10T00:00:00Z", Some(0), "rates held steady, page reworded"),
            page("https://c.example/merger", "c1", "2014-04-01T00:00:00Z", "2019-06-10T00:00:00Z", Some(2), "steady merger talk, later completed"),
            page("https://d.example/outlook", "d1", "2014-04-02T00:00:00Z", "2019-06-10T00:00:00Z", None, "steady outlook"),
        ]);
        put(store, &pages, "capture-2", "2019-06-11T00:00:00Z", second, &PAGE_TYPES);
        ids.push(contextful_context::build::manifests(store, "research/pages").unwrap().last().unwrap().snapshot_id.to_string());
    })
}

fn urls(answer: &Value) -> Vec<String> {
    let mut out: Vec<String> = ok(answer)["rows"].as_array().unwrap().iter().map(|r| r[0].as_str().unwrap().to_string()).collect();
    out.sort();
    out
}

/// `[pipeline.tables.snapshot]` declares {{type:contextful_core::store::declare::SnapshotDecl}}, the columns
/// describing each captured document. Each capture commits as one store snapshot, and an evaluation case pins
/// it by `snapshot` id.
// spec: store.snapshot.capture@1b5ecf0e
#[test]
fn each_capture_commits_one_snapshot_a_read_pins_by_id() {
    let mut ids = Vec::new();
    let f = captures(None, &mut ids);
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "each capture commits its own snapshot");
    let (s, _) = server(&f);
    let sql = r#"SELECT url, digest FROM "research/pages" ORDER BY url"#;

    let first = call(&s, "context.query", json!({ "sql": sql, "pin": { "research/pages": ids[0] } }));
    assert_eq!(ok(&first)["rows"], json!([["https://a.example/rates", "a1"], ["https://b.example/jobs", "b1"]]), "{first}");
    let second = call(&s, "context.query", json!({ "sql": sql, "pin": { "research/pages": ids[1] } }));
    assert_eq!(urls(&second), ["https://a.example/rates", "https://b.example/jobs", "https://c.example/merger", "https://d.example/outlook"]);

    let unknown = refused(&call(&s, "context.query", json!({ "sql": sql, "pin": { "research/pages": "0" } })));
    assert!(unknown.contains("PinnedBuildUnavailable") && unknown.contains("research/pages"), "{unknown}");
}

/// A document captured after the reading horizon carries `_available_reconstructed` true under {{store.available-
/// time.live-since}}. A horizon read admits the document only when the block's `leak_score` column holds 0.
// spec: store.snapshot.post-horizon-capture@81e4608c
#[test]
fn a_capture_after_the_horizon_answers_only_at_a_leak_score_of_zero() {
    let mut ids = Vec::new();
    let open = captures(None, &mut ids);
    let (s, _) = server(&open);
    let flags = call(&s, "context.query", json!({ "sql": r#"SELECT url, _available_reconstructed FROM "research/pages" WHERE captured_at > TIMESTAMPTZ '2014-05-01T00:00:00Z' ORDER BY url"# }));
    assert_eq!(
        ok(&flags)["rows"],
        json!([["https://a.example/rates", true], ["https://c.example/merger", true], ["https://d.example/outlook", true]]),
        "{flags}"
    );

    let mut ids = Vec::new();
    let bounded = captures(Some("2014-05-01T00:00:00Z"), &mut ids);
    let (s, _) = server(&bounded);
    // The reworded page scores 0 and answers; the merger page scores 2 and the outlook page
    // carries no score, so neither does.
    let answer = call(&s, "context.query", json!({ "sql": r#"SELECT url FROM "research/pages""# }));
    assert_eq!(urls(&answer), ["https://a.example/rates", "https://b.example/jobs"]);
}

/// Under a horizon, `corpus.retrieve` ranks stored rows through local indexes; search sources read their pinned
/// {{store.snapshot.capture}} corpus. Live search reaches no host through {{read.horizon.egress-hook}}.
// spec: read.horizon.local-rank@60b37b28
#[test]
fn a_horizon_ranking_reads_the_pinned_capture() {
    let mut ids = Vec::new();
    let f = captures(Some("2014-05-01T00:00:00Z"), &mut ids);
    let (s, _) = server(&f);
    let ranked = |pin: &str| {
        let answer = call(&s, "corpus.retrieve", json!({ "prefix": "research/pages", "query": "steady", "pin": { "research/pages": pin } }));
        let mut found: Vec<String> = ok(&answer)["rows"].as_array().unwrap().iter().map(|r| r[1]["url"].as_str().unwrap().to_string()).collect();
        found.sort();
        (answer, found)
    };
    // The first capture alone answers when pinned, though a later capture holds more pages.
    let (answer, found) = ranked(&ids[0]);
    assert_eq!(found, ["https://a.example/rates", "https://b.example/jobs"], "{answer}");
    let digests: Vec<&Value> = ok(&answer)["rows"].as_array().unwrap().iter().map(|r| &r[1]["digest"]).collect();
    assert_eq!(digests.len(), 2, "{answer}");
    assert!(digests.iter().all(|d| **d != json!("a2")), "{answer}");
    // The later capture answers within the horizon only where its leak score is 0.
    let (_, found) = ranked(&ids[1]);
    assert_eq!(found, ["https://a.example/rates", "https://b.example/jobs"]);
}

const PRICES: &str = r#"[[pipeline.tables]]
name = "research/prices"

[pipeline.tables.valid_time]
from = "date"

[pipeline.tables.available_time]
column = "available_at"
adjustment = { table = "research/splits", factor_column = "factor", adjust_columns = ["close"] }

[[pipeline.tables]]
name = "research/splits"
primary_key = ["ticker"]
content_hash_column = "digest"

[pipeline.tables.valid_time]
from = "ex_date"

[pipeline.tables.available_time]
column = "announced_at"
"#;

/// A 2014 price, a 2:1 split announced in April 2014 effective in June, and a 7:1 split
/// announced and effective in 2020, beside another ticker's price.
fn prices() -> Fixture {
    let decl = decls(PRICES);
    fixture_bounded(PRICES, None, |store| {
        let ts = [("date", ColumnType::Timestamp), ("available_at", ColumnType::Timestamp)];
        let rows = json!([
            { "ticker": "AAPL", "date": "2014-01-02T00:00:00Z", "available_at": "2014-01-02T21:00:00Z", "close": 553.13 },
            { "ticker": "AAPL", "date": "2014-07-01T00:00:00Z", "available_at": "2014-07-01T21:00:00Z", "close": 93.52 },
            { "ticker": "MSFT", "date": "2014-01-02T00:00:00Z", "available_at": "2014-01-02T21:00:00Z", "close": 37.16 },
        ]);
        put(store, &decl("research/prices"), "run-1", "2026-01-01T00:00:00Z", rows, &ts);
        let ts = [("ex_date", ColumnType::Timestamp), ("announced_at", ColumnType::Timestamp)];
        let splits = json!([
            { "ticker": "AAPL", "ex_date": "2014-06-09T00:00:00Z", "announced_at": "2014-04-23T20:30:00Z", "factor": 0.5, "digest": "s1" },
            { "ticker": "AAPL", "ex_date": "2020-08-31T00:00:00Z", "announced_at": "2020-07-30T20:30:00Z", "factor": 0.25, "digest": "s2" },
        ]);
        put(store, &decl("research/splits"), "run-1", "2026-01-01T00:00:00Z", splits, &ts);
    })
}

fn closes(s: &Server<'_>, available_as_of: Option<&str>) -> Value {
    let mut arguments = json!({ "sql": r#"SELECT ticker, CAST(date AS VARCHAR), CAST(close AS DOUBLE) FROM "research/prices" ORDER BY ticker, date"# });
    if let Some(bound) = available_as_of {
        arguments["available_as_of"] = json!(bound);
    }
    ok(&call(s, "context.query", arguments))["rows"].clone()
}

fn close_of(rows: &Value, i: usize) -> f64 {
    rows[i][2].as_f64().unwrap_or_else(|| rows[i][2].as_str().unwrap().parse().unwrap())
}

/// `available_time.adjustment` keeps stored values raw; an `available_as_of` read multiplies each `adjust_columns`
/// column by the product of `factor_column` vintages available by the bound, over factor rows sharing its factor-table
/// `primary_key` values with a later valid-time `from`.
// spec: store.available-time.adjustment-factors@c44a2c6d
#[test]
fn a_bounded_read_applies_only_factors_announced_by_the_bound() {
    let f = prices();
    let (s, _) = server(&f);
    // Unbounded, the stored price stays raw.
    assert_eq!(close_of(&closes(&s, None), 0), 553.13);
    // Before the 2014 split was announced, nothing adjusts.
    let march = closes(&s, Some("2014-03-01T00:00:00Z"));
    assert_eq!(close_of(&march, 0), 553.13);
    // After its announcement, the January price halves; the 2020 split stays unknown.
    let may = closes(&s, Some("2014-05-01T00:00:00Z"));
    assert!((close_of(&may, 0) - 276.565).abs() < 1e-9, "{may}");
    // In 2021 both splits are known.
    let later = closes(&s, Some("2021-01-01T00:00:00Z"));
    assert!((close_of(&later, 0) - 553.13 * 0.5 * 0.25).abs() < 1e-9, "{later}");
}

/// `available_time.adjustment` keeps stored values raw; an `available_as_of` read multiplies each `adjust_columns` column by the product of `factor_column`
/// vintages available by the bound, over factor rows sharing its factor-table `primary_key` values with a later valid-time `from`.
// spec: store.available-time.adjustment-factors@c44a2c6d
#[test]
fn a_factor_applies_to_its_own_key_before_its_effective_date() {
    let f = prices();
    let (s, _) = server(&f);
    let rows = closes(&s, Some("2014-12-31T00:00:00Z"));
    // AAPL before the June ex-date halves; AAPL after it does not; MSFT shares no key.
    assert!((close_of(&rows, 0) - 276.565).abs() < 1e-9, "{rows}");
    assert!((close_of(&rows, 1) - 93.52).abs() < 1e-9, "{rows}");
    assert!((close_of(&rows, 2) - 37.16).abs() < 1e-9, "{rows}");
}
