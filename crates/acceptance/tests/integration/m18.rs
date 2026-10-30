//! Milestone 18 — provider orchestrators.
//!
//! Reach: a backfill chunk's provider step crashed after half its pages resumes under an
//! in-tree test filler and lands parts byte-identical to the engine runner's.

use contextful_acceptance::http::{Request, Response, Server};
use contextful_acceptance::orchestrate::{Filler, Unit};
use contextful_acceptance::{bin, GitRepo};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const STORE: &str = ".contextful/context/research";
const T0: &str = "2030-01-01T00:00:00Z";
/// Past the 30 s owner lease of an attempt opened at [`T0`].
const T1: &str = "2030-01-01T00:05:00Z";

/// The vendor's knobs: the export page a GET holds unanswered, whether a GET of it arrived, and
/// how many GETs answer `429` with `Retry-After: 1` first.
#[derive(Default)]
struct Faults {
    hang_page: AtomicUsize,
    hung: AtomicBool,
    throttles: AtomicUsize,
}

/// `/exports/items?page=N` serves four pages of two rows, then an empty page; `/v1/items?page=N`
/// serves one row, then an empty page.
fn vendor(faults: Arc<Faults>) -> Server {
    Server::start(move |request: &Request| {
        let page: usize = request.query("page").and_then(|p| p.parse().ok()).unwrap_or(1);
        let export = request.path().starts_with("/exports/");
        if request.method == "GET" && export && faults.hang_page.load(Ordering::SeqCst) == page {
            faults.hung.store(true, Ordering::SeqCst);
            while faults.hang_page.load(Ordering::SeqCst) == page {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if request.method == "GET" && faults.throttles.load(Ordering::SeqCst) > 0 {
            faults.throttles.fetch_sub(1, Ordering::SeqCst);
            return Response { status: 429, headers: vec![("Retry-After".into(), "1".into())], body: Vec::new() };
        }
        let body = match (export, page) {
            (true, 1..=4) => format!(r#"[{{"id":"e{page}a","updated_at":{}}},{{"id":"e{page}b","updated_at":{}}}]"#, page * 10, page * 10 + 1),
            (false, 1) => r#"[{"id":"live","updated_at":150}]"#.to_string(),
            _ => "[]".to_string(),
        };
        let mut response = Response::json(200, &body);
        response.headers.push(("ETag".into(), "\"v1\"".into()));
        if request.method == "HEAD" {
            response.body.clear();
        }
        response
    })
}

fn project(server: &Server) -> GitRepo {
    let p = GitRepo::init();
    p.write(&format!("{STORE}/config.toml"), "[node]\nid = \"ingest-a\"\n");
    p.write(
        "contextful.toml",
        &format!(
            "authoring_posture = \"per_request\"\n\
             [[pipeline]]\nid = \"orders\"\n[[pipeline.tables]]\nname = \"items\"\nprimary_key = [\"id\"]\norder_by = \"updated_at\"\n\
             [pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\", page_param = \"page\" }}\n\
             [pipeline.seed]\nbelow = 100\n[pipeline.seed.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\", page_param = \"page\" }}\n\
             \n[[job]]\nname = \"fold-items\"\nschedule = \"0 3 * * *\"\nkind = \"fold\"\ntarget = \"orders_items\"\n",
            server.url("/v1/{table}"),
            server.url("/exports/{table}"),
        ),
    );
    p
}

/// The committed Parquet parts of `run`'s commit in `orders_items`, as `(relative path, bytes)`.
fn parts(cf: &Path, p: &GitRepo, run: &str) -> Vec<(String, Vec<u8>)> {
    let out = p.run(cf, &["context", "files", "orders_items", "--project", "research"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let prefix = format!("tables/orders_items/data/runs/{run}/");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|f| f.starts_with(&prefix))
        .map(|f| (f.to_string(), std::fs::read(p.root.join(STORE).join(f)).unwrap()))
        .collect()
}

fn targets(server: &Server) -> Vec<String> {
    std::mem::take(&mut *server.requests.lock().unwrap()).into_iter().map(|r| format!("{} {}", r.method, r.target)).collect()
}

#[test]
fn m18_orchestrate() {
    let cf = bin("contextful");
    let faults: Arc<Faults> = Arc::default();
    let server = vendor(faults.clone());
    let p = project(&server);
    let mut provider = Filler::new(&cf, &p.root, "orders", "research", "site-a");
    let chunk = Unit::Chunk(0);

    // The chunk's step dies after half its pages: pages 1 and 2 stand recorded, page 3 does not.
    faults.hang_page.store(3, Ordering::SeqCst);
    provider.step_killed("wf-1", &chunk, T0, || faults.hung.load(Ordering::SeqCst));
    faults.hang_page.store(0, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    targets(&server);

    // The provider replays the step once the dead attempt's lease lapsed; the engine resumes from its recorded pages.
    let pointer = provider.step("wf-1", &chunk, T1).expect("the replayed chunk step completes");
    assert_eq!(pointer, "orders_items chunk 0: commit wf-1.replay-2.seed-0");
    let sent = targets(&server);
    assert!(sent.iter().any(|t| t == "GET /exports/items?page=3"), "{sent:?}");
    assert!(!sent.iter().any(|t| t.ends_with("page=1") || t.ends_with("page=2")), "no recorded page is sent again: {sent:?}");

    // A replay of the completed step returns its pointer and re-enters nothing.
    assert_eq!(provider.step("wf-1", &chunk, T1).as_deref(), Ok(pointer.as_str()));
    assert!(targets(&server).is_empty(), "a completed step's replay sends no request");
    assert_eq!(provider.record("wf-1").map(|r| r.invocations), Some(3));

    // The engine runner's uninterrupted fire, naming its chunk run as the resumed attempt is named, lands the same bytes.
    let runner = project(&server);
    let out = runner.run(&cf, &["pipeline", "run", "orders", "--project", "research", "--run-id", "wf-1.replay-2", "--site-id", "site-a", "--now", T1]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let (stepped, ran) = (parts(&cf, &p, "wf-1.replay-2.seed-0"), parts(&cf, &runner, "wf-1.replay-2.seed-0"));
    assert!(!stepped.is_empty(), "the resumed chunk committed parts");
    assert_eq!(stepped, ran, "the resumed chunk's parts match the engine runner's byte for byte");
    targets(&server);

    // A throttled live pull retries on the engine's schedule inside its step; the provider records one invocation.
    faults.throttles.store(1, Ordering::SeqCst);
    let live = provider.step("wf-2", &Unit::Table("items".into()), T1).expect("the throttled table step completes");
    assert_eq!(live, "orders_items: commit wf-2");
    assert_eq!(provider.record("wf-2").map(|r| r.invocations), Some(1));
    let sent = targets(&server);
    assert_eq!(sent.iter().filter(|t| *t == "GET /v1/items?page=1").count(), 2, "the engine retried the throttled page: {sent:?}");
}
