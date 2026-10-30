//! The `search` source through the built binary, against a loopback provider that pages each
//! query's results by cursor.
#![cfg(feature = "data-plane")]

use crate::pipeline::{cf, ok, project, stderr, Vendor};
use std::path::Path;
use std::process::Output;
use std::sync::{Arc, Mutex};

/// The provider's answer to `target`: `rust lang` holds two pages, `p2` naming the second, and
/// `web assembly` one; a target holding a `failing` marker answers that marker's status.
fn answer(target: &str, failing: &[(String, u16)]) -> (u16, String) {
    let result = |url: &str, at: &str| serde_json::json!({"url": url, "title": url.trim_start_matches("https://"), "published_at": at});
    if let Some((_, status)) = failing.iter().find(|(marker, _)| target.contains(marker.as_str())) {
        return (*status, "{}".into());
    }
    let body = if target.contains("q=rust%20lang") && target.contains("cursor=p2") {
        serde_json::json!({"results": [result("https://r3.test", "2030-01-09T11:00:00Z")]})
    } else if target.contains("q=rust%20lang") {
        serde_json::json!({"results": [result("https://r1.test", "2030-01-09T10:00:00Z"), result("https://r2.test", "2030-01-09T12:00:00Z")], "next": "p2"})
    } else if target.contains("q=web%20assembly") {
        serde_json::json!({"results": [result("https://w1.test", "2030-01-08T09:00:00Z")]})
    } else {
        return (404, "{}".into());
    };
    (200, body.to_string())
}

struct Provider {
    vendor: Vendor,
    failing: Arc<Mutex<Vec<(String, u16)>>>,
}

impl Provider {
    fn start() -> Provider {
        let failing: Arc<Mutex<Vec<(String, u16)>>> = Arc::default();
        let state = failing.clone();
        let vendor = Vendor::start(move |target| answer(target, &state.lock().unwrap()));
        Provider { vendor, failing }
    }

    /// Answer 503 to every target holding `marker`.
    fn fail(&self, marker: &str) {
        self.answer_with(marker, 503);
    }

    fn answer_with(&self, marker: &str, status: u16) {
        *self.failing.lock().unwrap() = vec![(marker.to_string(), status)];
    }

    fn heal(&self) {
        self.failing.lock().unwrap().clear();
    }
}

fn manifest(provider: &Provider, extra: &str) -> String {
    format!(
        "[[pipeline]]\nid = \"news\"\ntables = [{{ name = \"results\", primary_key = [\"search_query\", \"url\"] }}]\n[pipeline.source]\nname = \"search\"\n[pipeline.source.config]\nurl = \"{}\"\nqueries = {{ rust = \"rust lang\", wasm = \"web assembly\" }}\nfreshness = \"2d\"\n{extra}\n",
        provider.vendor.url("/search?q={query}&since={since}")
    )
}

fn fire(dir: &Path, run: &str, now: &str) -> Output {
    cf(dir, &["pipeline", "run", "news", "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now])
}

fn landed(dir: &Path) -> Vec<(String, String)> {
    let sql = "SELECT search_query, url FROM news_results ORDER BY search_query, url";
    let out: serde_json::Value = serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", sql]))).unwrap();
    out["rows"].as_array().cloned().unwrap_or_default().iter().map(|r| (r[0].as_str().unwrap_or_default().to_string(), r[1].as_str().unwrap_or_default().to_string())).collect()
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items.iter().map(|(q, u)| (q.to_string(), u.to_string())).collect()
}

/// A fire pages each query by the cursor its pages name, lands one row per result carrying its query's name, and
/// holds one watermark per query; the next fire's `{since}` fills from each query's own watermark, a first fire's
/// from its instant less `freshness`.
// spec: connector.source.search-config@67a609dd
// spec: connector.source.search-templates@acf1de13
// spec: connector.source.search-paging@95abb729
// spec: connector.source.search-watermark@d2515c4a
#[test]
fn a_search_run_pages_each_query_and_keeps_a_watermark_per_query() {
    let provider = Provider::start();
    let dir = project(&manifest(&provider, "max_results = 10"));
    ok(&cf(dir.path(), &["pipeline", "validate"]));
    assert!(provider.vendor.targets().is_empty(), "validation reaches no provider");

    let out = ok(&fire(dir.path(), "f1", "2030-01-10T00:00:00Z"));
    assert!(out.contains("news_results: f1 success · 4 rows"), "{out}");
    assert_eq!(
        provider.vendor.targets(),
        [
            "/search?q=rust%20lang&since=2030-01-08T00%3A00%3A00Z",
            "/search?q=rust%20lang&since=2030-01-08T00%3A00%3A00Z&cursor=p2",
            "/search?q=web%20assembly&since=2030-01-08T00%3A00%3A00Z",
        ]
    );
    assert_eq!(
        landed(dir.path()),
        pairs(&[("rust", "https://r1.test"), ("rust", "https://r2.test"), ("rust", "https://r3.test"), ("wasm", "https://w1.test")])
    );

    ok(&fire(dir.path(), "f2", "2030-01-11T00:00:00Z"));
    assert_eq!(
        provider.vendor.targets()[3..],
        [
            "/search?q=rust%20lang&since=2030-01-09T12%3A00%3A00Z",
            "/search?q=rust%20lang&since=2030-01-09T12%3A00%3A00Z&cursor=p2",
            "/search?q=web%20assembly&since=2030-01-08T09%3A00%3A00Z",
        ]
    );
}

/// A pass reaching `max_results` stops paging and finishes, its watermark advancing over what it landed.
// spec: connector.source.search-paging@95abb729
#[test]
fn a_search_pass_stops_at_its_result_ceiling() {
    let provider = Provider::start();
    let dir = project(&manifest(&provider, "max_results = 2"));
    let out = ok(&fire(dir.path(), "f1", "2030-01-10T00:00:00Z"));
    assert!(out.contains("3 rows"), "{out}");
    let targets = provider.vendor.targets();
    assert!(!targets.is_empty());
    assert!(targets.iter().all(|t| !t.contains("cursor=")), "the ceiling ends the pass before its second page: {targets:?}");
    assert_eq!(landed(dir.path()), pairs(&[("rust", "https://r1.test"), ("rust", "https://r2.test"), ("wasm", "https://w1.test")]));
    ok(&fire(dir.path(), "f2", "2030-01-11T00:00:00Z"));
    assert_eq!(provider.vendor.targets()[2], "/search?q=rust%20lang&since=2030-01-09T12%3A00%3A00Z");
}

/// A provider 503 on one query's second page holds that query at its committed cursor and lands the other query.
/// The next fire resumes the held pass at its cursor under the `{since}` it began with, and the watermark the
/// finished pass sets covers every page it landed.
// spec: connector.source.search-degradation@487652bd
// spec: connector.source.search-resume@b4107625
// spec: connector.source.search-watermark@d2515c4a
#[test]
fn a_resumed_search_continues_from_its_cursor_and_then_its_watermark() {
    let provider = Provider::start();
    let dir = project(&manifest(&provider, "max_results = 10"));
    provider.fail("cursor=p2");
    let out = ok(&fire(dir.path(), "f1", "2030-01-10T00:00:00Z"));
    assert!(out.contains("news_results: f1 success · 3 rows"), "a failed page holds its query and lands the rest: {out}");
    assert_eq!(landed(dir.path()), pairs(&[("rust", "https://r1.test"), ("rust", "https://r2.test"), ("wasm", "https://w1.test")]));

    provider.heal();
    let out = ok(&fire(dir.path(), "f2", "2030-01-11T00:00:00Z"));
    assert!(out.contains("2 rows"), "{out}");
    assert_eq!(
        provider.vendor.targets()[3..],
        [
            "/search?q=rust%20lang&since=2030-01-08T00%3A00%3A00Z&cursor=p2",
            "/search?q=web%20assembly&since=2030-01-08T09%3A00%3A00Z",
        ]
    );
    assert!(landed(dir.path()).contains(&("rust".to_string(), "https://r3.test".to_string())));

    ok(&fire(dir.path(), "f3", "2030-01-12T00:00:00Z"));
    assert_eq!(provider.vendor.targets()[5], "/search?q=rust%20lang&since=2030-01-09T12%3A00%3A00Z");
}

/// A cleartext URL off loopback fails validation with `ConnectorProviderOriginRejected`; a malformed template or
/// key fails validation naming it, before any request.
// spec: connector.source.provider-origin@a0e63ea1
// spec: connector.source.search-templates@acf1de13
// spec: connector.source.search-config@67a609dd
#[test]
fn a_malformed_search_declaration_fails_validation_before_any_request() {
    let provider = Provider::start();
    let good = manifest(&provider, "max_results = 10");
    ok(&cf(project(&good).path(), &["pipeline", "validate"]));
    let host = format!("127.0.0.1:{}", provider.vendor.url("").trim_start_matches("http://127.0.0.1:"));
    let cases = [
        (good.replace(&format!("http://{host}"), "http://search.test"), "ConnectorProviderOriginRejected"),
        (good.replace("&since={since}", ""), "{since}"),
        (good.replace("{since}", "{until}"), "{until}"),
        (good.replace(&format!("http://{host}"), "https://{query}.test"), "host"),
        (good.replace("max_results = 10", "max_results = 0"), "max_results"),
        (good.replace("freshness = \"2d\"", "freshness = \"soon\""), "freshness"),
        (good.replace("queries = { rust = \"rust lang\", wasm = \"web assembly\" }", "queries = {}"), "queries"),
        (good.replace("max_results = 10", "max_results = 10\nlimit = 3"), "limit"),
    ];
    for (declaration, key) in cases {
        assert_ne!(declaration, good, "the case for `{key}` changes the declaration");
        let out = cf(project(&declaration).path(), &["pipeline", "validate"]);
        assert!(!out.status.success(), "{key}: {}", String::from_utf8_lossy(&out.stdout));
        assert!(stderr(&out).contains(key), "{key}: {}", stderr(&out));
    }
    assert!(provider.vendor.targets().is_empty(), "validation reaches no provider: {:?}", provider.vendor.targets());
}

/// A fire failing permanently on its third pull holds its execution; the next fire replays the two journaled pulls of
/// `rust lang` without asking the provider again, then reads `web assembly` live and ends, landing what an
/// uninterrupted fire lands.
// spec: run.journal.replay-lands@0cc34691
#[test]
fn a_resumed_search_fire_replays_its_journaled_result_sets_without_asking_the_provider() {
    let provider = Provider::start();
    let dir = project(&manifest(&provider, "max_results = 10"));
    provider.answer_with("q=web%20assembly", 400);
    let failed = fire(dir.path(), "f1", "2030-01-10T00:00:00Z");
    assert!(!failed.status.success(), "a 400 fails the fire: {}", String::from_utf8_lossy(&failed.stdout));
    assert_eq!(provider.vendor.targets().len(), 3, "{:?}", provider.vendor.targets());

    provider.heal();
    let out = ok(&fire(dir.path(), "f2", "2030-01-10T00:01:00Z"));
    assert!(out.contains("4 rows"), "{out}");
    // The live pull fills `{since}` from the resuming fire's instant.
    assert_eq!(provider.vendor.targets()[3..], ["/search?q=web%20assembly&since=2030-01-08T00%3A01%3A00Z"], "the replay re-reads no recorded page");
    assert_eq!(
        landed(dir.path()),
        pairs(&[("rust", "https://r1.test"), ("rust", "https://r2.test"), ("rust", "https://r3.test"), ("wasm", "https://w1.test")])
    );
}

/// A provider key reference no assembled adapter answers fails the fire with `SecretUnresolvedReference` naming it,
/// before any request, rather than landing a zero-row success.
// spec: connector.resolve.unresolved-name@b7bc2a10
#[test]
fn an_unresolved_provider_key_fails_the_search_fire_before_any_request() {
    let provider = Provider::start();
    let key = "max_results = 10\nheaders = { Authorization = \"Bearer ${secret://search-provider-key}\" }";
    let dir = project(&manifest(&provider, key));
    let out = fire(dir.path(), "f1", "2030-01-10T00:00:00Z");
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let why = stderr(&out);
    assert!(why.contains("SecretUnresolvedReference") && why.contains("search-provider-key"), "{why}");
    assert!(provider.vendor.targets().is_empty(), "no request leaves: {:?}", provider.vendor.targets());
}
