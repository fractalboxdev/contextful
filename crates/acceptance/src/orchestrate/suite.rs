//! The conformance suite of the orchestrator role (`run.orchestrate.semantics`): the scenarios
//! a filler passes before a target profile names it. Each drives the stock `contextful` binary
//! through the filler under test against a loopback vendor or delegate, and observes the
//! engine's record through `run show` and `context files`:
//!
//! - `step-replay`: a replay of a completed step answers its recorded pointer and sends nothing.
//! - `resume`: a chunk step crashed after half its pages resumes from its recorded pages and
//!   lands the engine runner's bytes.
//! - `lease-per-step`: a step holds its owner lease only while it runs, and a replay inside a
//!   dead attempt's lease refuses naming it.
//! - `one-retry-layer`: a throttle retries on the engine's schedule inside one invocation, and a
//!   failed step stays failed.
//! - `stop`: a stop on a step's run closes it `canceled`, and a replay resumes the chunk.
//! - `delegate-inside`: a job step's `call` delegate runs under the engine's entry key, and a
//!   replay sends only the unanswered call.
//!
//! No scenario suspends on an awakeable: the stock binary's only suspending body, a `submit`
//! delegate, refuses under `surface.delegate.submit-unfilled`.

use super::Unit;
use crate::delegate::{request_signed, signed_answer};
use crate::http::{Request, Response, Server};
use crate::GitRepo;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const STORE: &str = ".contextful/context/research";
const PROJECT: &str = "research";
const SITE: &str = "site-a";
const T0: &str = "2030-01-01T00:00:00Z";
/// Past the 30 s owner lease of an attempt opened at [`T0`].
const T1: &str = "2030-01-01T00:05:00Z";
const AUDIENCE: &str = "contextful://acme-research";
const KEY: &[u8] = b"a-deployment-worker-key";
const ROUTE: &str = "/stage/screen";

/// A filler of the orchestrator role, as the suite drives it.
pub trait Orchestrator {
    /// Invoke step `id` of `unit` at `now`, a first run or a replay; answers the step's commit
    /// pointer or its failure.
    fn step(&mut self, id: &str, unit: &Unit, now: &str) -> Result<String, String>;
    /// Invoke step `id` at `now` and kill its process once `reached` holds: a crash inside the step.
    fn step_killed(&mut self, id: &str, unit: &Unit, now: &str, reached: &dyn Fn() -> bool);
    /// Invoke step `id` at `now`, run `meanwhile` once `reached` holds, and answer as [`Orchestrator::step`].
    fn step_during(&mut self, id: &str, unit: &Unit, now: &str, reached: &dyn Fn() -> bool, meanwhile: &mut dyn FnMut()) -> Result<String, String>;
    /// The invocations the provider made of step `id`: the first and every replay.
    fn invocations(&self, id: &str) -> u32;
}

/// The project a filler orchestrates.
#[derive(Debug, Clone)]
pub struct Target {
    pub bin: PathBuf,
    pub root: PathBuf,
    pub pipeline: String,
    pub project: String,
    pub site: String,
    /// Environment every step process receives.
    pub env: Vec<(String, String)>,
    /// Arguments a job step's `job fire` receives before the step's own.
    pub job_args: Vec<String>,
}

/// Builds the filler under test over a target.
pub type Make = dyn Fn(&Target) -> Box<dyn Orchestrator> + Sync;

/// One scenario: `Ok` when the filler built by the second argument passes it under `bin`.
pub type Scenario = fn(&Path, &Make) -> Result<(), String>;

/// Every scenario of the suite, by name.
pub const SCENARIOS: &[(&str, Scenario)] = &[
    ("step-replay", step_replay),
    ("resume", resume),
    ("lease-per-step", lease_per_step),
    ("one-retry-layer", one_retry_layer),
    ("stop", stop),
    ("delegate-inside", delegate_inside),
];

/// Run every scenario concurrently against the filler `make` builds; a panicking scenario fails.
pub fn run(bin: &Path, make: &Make) -> Vec<(&'static str, Result<(), String>)> {
    std::thread::scope(|scope| {
        let running: Vec<_> = SCENARIOS.iter().map(|(name, scenario)| (*name, scope.spawn(move || scenario(bin, make)))).collect();
        running
            .into_iter()
            .map(|(name, handle)| (name, handle.join().unwrap_or_else(|panic| Err(format!("panicked: {}", panic_text(&panic))))))
            .collect()
    })
}

fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic.downcast_ref::<String>().cloned().or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
}

fn ensure(holds: bool, diagnostic: impl FnOnce() -> String) -> Result<(), String> {
    if holds {
        Ok(())
    } else {
        Err(diagnostic())
    }
}

/// The vendor's faults: the export page a GET holds unanswered and whether one arrived, how
/// many GETs answer `429` with `Retry-After: 1` first, and whether every GET answers `400`.
#[derive(Default)]
struct Faults {
    hang_page: AtomicUsize,
    hung: AtomicBool,
    throttles: AtomicUsize,
    refuse: AtomicBool,
}

/// A loopback vendor. `/exports/items?page=N` serves four pages of two rows, then an empty page;
/// `/v1/items?page=N` serves one row, then an empty page.
struct Vendor {
    server: Server,
    faults: Arc<Faults>,
}

impl Vendor {
    fn start() -> Vendor {
        let faults: Arc<Faults> = Arc::default();
        let held = faults.clone();
        let server = Server::start(move |request: &Request| {
            let page: usize = request.query("page").and_then(|p| p.parse().ok()).unwrap_or(1);
            let export = request.path().starts_with("/exports/");
            if request.method == "GET" && export && held.hang_page.load(Ordering::SeqCst) == page {
                held.hung.store(true, Ordering::SeqCst);
                while held.hang_page.load(Ordering::SeqCst) == page {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            if request.method == "GET" && held.throttles.load(Ordering::SeqCst) > 0 {
                held.throttles.fetch_sub(1, Ordering::SeqCst);
                return Response { status: 429, headers: vec![("Retry-After".into(), "1".into())], body: Vec::new() };
            }
            if request.method == "GET" && held.refuse.load(Ordering::SeqCst) {
                return Response::json(400, "{}");
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
        });
        Vendor { server, faults }
    }

    /// Every request since the last call, as `<method> <target>`.
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.server.requests.lock().unwrap()).into_iter().map(|r| format!("{} {}", r.method, r.target)).collect()
    }

    /// Hold export page `page` until [`Vendor::release`]; `hung` holds once its GET arrives.
    fn hang(&self, page: usize) {
        self.faults.hung.store(false, Ordering::SeqCst);
        self.faults.hang_page.store(page, Ordering::SeqCst);
    }

    fn hung(&self) -> bool {
        self.faults.hung.load(Ordering::SeqCst)
    }

    /// Answer the held page, then let its request settle into the record.
    fn release(&self) {
        self.faults.hang_page.store(0, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A scenario's project and the target a filler orchestrates over it.
struct Project {
    repo: GitRepo,
    target: Target,
}

impl Project {
    fn new(bin: &Path, pipeline: &str, manifest: &str) -> Project {
        let repo = GitRepo::init();
        repo.write(&format!("{STORE}/config.toml"), "[node]\nid = \"ingest-a\"\n");
        repo.write("contextful.toml", manifest);
        let target = Target { bin: bin.to_path_buf(), root: repo.root.clone(), pipeline: pipeline.into(), project: PROJECT.into(), site: SITE.into(), env: Vec::new(), job_args: Vec::new() };
        Project { repo, target }
    }

    /// Pipeline `orders` landing `orders_items` from the live feed, seeded from the export below 100.
    fn seeded(bin: &Path, vendor: &Vendor) -> Project {
        Project::new(
            bin,
            "orders",
            &format!(
                "authoring_posture = \"per_request\"\n\
                 [[pipeline]]\nid = \"orders\"\n[[pipeline.tables]]\nname = \"items\"\nprimary_key = [\"id\"]\norder_by = \"updated_at\"\n\
                 [pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\", page_param = \"page\" }}\n\
                 [pipeline.seed]\nbelow = 100\n[pipeline.seed.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\", page_param = \"page\" }}\n\
                 \n[[job]]\nname = \"fold-items\"\nschedule = \"0 3 * * *\"\nkind = \"fold\"\ntarget = \"orders_items\"\n",
                vendor.server.url("/v1/{table}"),
                vendor.server.url("/exports/{table}"),
            ),
        )
    }

    /// Pipeline `feed` landing `feed_items` from the live feed alone.
    fn live(bin: &Path, vendor: &Vendor) -> Project {
        Project::new(
            bin,
            "feed",
            &format!(
                "authoring_posture = \"per_request\"\n\
                 [[pipeline]]\nid = \"feed\"\n[[pipeline.tables]]\nname = \"items\"\n\
                 [pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\", page_param = \"page\" }}\n",
                vendor.server.url("/v1/{table}"),
            ),
        )
    }

    fn cf(&self, args: &[&str]) -> Output {
        let env: Vec<(&str, &str)> = self.target.env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        self.repo.run_env(&self.target.bin, args, &env)
    }

    fn ok(&self, args: &[&str]) -> Result<String, String> {
        let out = self.cf(args);
        ensure(out.status.success(), || format!("`{}` failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr)))?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Run `run`'s row, or `None` when the catalog holds no such run.
    fn show(&self, run: &str) -> Result<Option<Value>, String> {
        let out = self.cf(&["run", "show", run, "--project", PROJECT]);
        if !out.status.success() {
            return Ok(None);
        }
        serde_json::from_slice(&out.stdout).map(Some).map_err(|e| format!("`run show {run}` printed no row: {e}"))
    }

    fn status(&self, run: &str) -> Result<Value, String> {
        Ok(self.show(run)?.map(|row| row["status"].clone()).unwrap_or(Value::Null))
    }

    /// The committed Parquet parts of `run`'s commit in `table`, as `(relative path, bytes)`.
    fn parts(&self, table: &str, run: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
        let listed = self.ok(&["context", "files", table, "--project", PROJECT])?;
        let prefix = format!("tables/{table}/data/runs/{run}/");
        listed
            .lines()
            .filter(|f| f.starts_with(&prefix))
            .map(|f| std::fs::read(self.repo.root.join(STORE).join(f)).map(|bytes| (f.to_string(), bytes)).map_err(|e| format!("{f}: {e}")))
            .collect()
    }

    /// The rows `sql` answers, each as its cells' text.
    fn select(&self, sql: &str) -> Result<Vec<Vec<String>>, String> {
        let answer: Value = serde_json::from_str(&self.ok(&["query", "--json", "--project", PROJECT, sql])?).map_err(|e| e.to_string())?;
        let rows = answer["rows"].as_array().ok_or_else(|| format!("`{sql}` answered no rows: {answer}"))?;
        Ok(rows.iter().map(|r| r.as_array().into_iter().flatten().map(|c| c.as_str().map(str::to_string).unwrap_or_else(|| c.to_string())).collect()).collect())
    }
}

/// Kill chunk 0's step `wf-1` at [`T0`] once the vendor holds export page 3, so pages 1 and 2
/// stand recorded and page 3 does not.
fn kill_after_half(provider: &mut dyn Orchestrator, vendor: &Vendor) {
    vendor.hang(3);
    provider.step_killed("wf-1", &Unit::Chunk(0), T0, &|| vendor.hung());
    vendor.release();
}

/// A replay of a completed table step answers its recorded pointer and re-enters nothing.
pub fn step_replay(bin: &Path, make: &Make) -> Result<(), String> {
    let vendor = Vendor::start();
    let p = Project::live(bin, &vendor);
    let mut provider = make(&p.target);
    let items = Unit::Table("items".into());
    let first = provider.step("wf-1", &items, T0)?;
    ensure(first == "feed_items: commit wf-1", || format!("the step's pointer: {first}"))?;
    let sent = vendor.take();
    ensure(sent == ["GET /v1/items?page=1", "GET /v1/items?page=2"], || format!("one pull pages the feed: {sent:?}"))?;
    ensure(p.status("wf-1")? == "success", || "the step's run closes success".into())?;

    let replayed = provider.step("wf-1", &items, T1)?;
    ensure(replayed == first, || format!("a replay answers the recorded pointer: {replayed}"))?;
    let sent = vendor.take();
    ensure(sent.is_empty(), || format!("a completed step's replay re-enters nothing: {sent:?}"))?;
    ensure(p.show("wf-1.replay-2")?.is_none(), || "a completed step's replay opens no attempt".into())?;
    ensure(provider.invocations("wf-1") == 2, || "the provider invoked the step twice".into())
}

/// A chunk step crashed after half its pages resumes from its recorded pages on replay and lands
/// parts byte-identical to the engine runner's uninterrupted fire.
pub fn resume(bin: &Path, make: &Make) -> Result<(), String> {
    let vendor = Vendor::start();
    let p = Project::seeded(bin, &vendor);
    let mut provider = make(&p.target);
    kill_after_half(provider.as_mut(), &vendor);
    let sent = vendor.take();
    ensure(sent.iter().any(|t| t == "GET /exports/items?page=2"), || format!("the crashed step recorded pages 1 and 2: {sent:?}"))?;

    let pointer = provider.step("wf-1", &Unit::Chunk(0), T1)?;
    ensure(pointer == "orders_items chunk 0: commit wf-1.replay-2.seed-0", || format!("the resumed pointer: {pointer}"))?;
    let sent = vendor.take();
    ensure(sent.iter().any(|t| t == "GET /exports/items?page=3"), || format!("the replay pulls the unrecorded page: {sent:?}"))?;
    ensure(!sent.iter().any(|t| t.ends_with("page=1") || t.ends_with("page=2")), || format!("no recorded page is sent again: {sent:?}"))?;
    ensure(p.show("wf-1.replay-2.seed-0")?.is_some_and(|row| row["rows"] == 8), || "the resumed chunk lands all eight rows".into())?;

    let again = provider.step("wf-1", &Unit::Chunk(0), T1)?;
    ensure(again == pointer, || format!("a replay of the completed chunk answers its pointer: {again}"))?;
    let sent = vendor.take();
    ensure(sent.is_empty(), || format!("and sends nothing, the fingerprint probe included: {sent:?}"))?;

    // The reference implementation: the engine runner names its chunk run as the resumed attempt is named.
    let runner = Project::seeded(bin, &vendor);
    runner.ok(&["pipeline", "run", "orders", "--project", PROJECT, "--run-id", "wf-1.replay-2", "--site-id", SITE, "--now", T1])?;
    let (stepped, ran) = (p.parts("orders_items", "wf-1.replay-2.seed-0")?, runner.parts("orders_items", "wf-1.replay-2.seed-0")?);
    ensure(!stepped.is_empty(), || "the resumed chunk committed parts".into())?;
    ensure(stepped == ran, || "the resumed chunk's parts match the engine runner's byte for byte".into())?;
    vendor.take();
    Ok(())
}

/// A step holds its attempt's owner lease only while it runs; a replay inside a dead attempt's
/// lease refuses naming that attempt and opens none, and a replay after the lapse resumes.
pub fn lease_per_step(bin: &Path, make: &Make) -> Result<(), String> {
    let vendor = Vendor::start();
    let p = Project::seeded(bin, &vendor);
    let mut provider = make(&p.target);
    kill_after_half(provider.as_mut(), &vendor);
    let dead = p.show("wf-1.seed-0")?.ok_or("the crashed attempt holds a run row")?;
    ensure(dead["status"] == "running" && !dead["owner"].is_null(), || format!("the crashed attempt still holds its lease: {dead}"))?;

    let early = provider.step("wf-1", &Unit::Chunk(0), T0);
    ensure(early.as_ref().is_err_and(|e| e.contains("wf-1.seed-0")), || format!("a replay inside the lease refuses naming the attempt: {early:?}"))?;
    ensure(p.show("wf-1.replay-2.seed-0")?.is_none(), || "a refused replay opens no attempt".into())?;
    vendor.take();

    let late = provider.step("wf-1", &Unit::Chunk(0), T1)?;
    ensure(late == "orders_items chunk 0: commit wf-1.replay-2.seed-0", || format!("a replay after the lapse resumes: {late}"))?;
    let row = p.show("wf-1.replay-2.seed-0")?.ok_or("the resumed attempt holds a run row")?;
    ensure(row["status"] == "success" && row["owner"].is_null(), || format!("a returned step holds no owner lease: {row}"))?;
    ensure(provider.invocations("wf-1") == 3, || "the provider invoked the step three times".into())
}

/// A throttled pull retries on the engine's schedule inside one invocation; a refused pull closes
/// its table failed, and the provider invokes the failed step once.
pub fn one_retry_layer(bin: &Path, make: &Make) -> Result<(), String> {
    let vendor = Vendor::start();
    let p = Project::live(bin, &vendor);
    let mut provider = make(&p.target);
    let items = Unit::Table("items".into());
    vendor.faults.throttles.store(1, Ordering::SeqCst);
    let pointer = provider.step("wf-1", &items, T0)?;
    ensure(pointer == "feed_items: commit wf-1", || format!("the throttled step completes: {pointer}"))?;
    let sent = vendor.take();
    ensure(sent == ["GET /v1/items?page=1", "GET /v1/items?page=1", "GET /v1/items?page=2"], || format!("the engine retries the throttled page: {sent:?}"))?;
    ensure(provider.invocations("wf-1") == 1, || "one invocation holds the throttled step".into())?;
    ensure(p.show("wf-1.replay-2")?.is_none(), || "and one attempt".into())?;

    vendor.faults.refuse.store(true, Ordering::SeqCst);
    let refused = provider.step("wf-2", &items, T1);
    ensure(refused.is_err(), || format!("a refused pull fails its step: {refused:?}"))?;
    ensure(p.status("wf-2")? == "failed", || "the engine closes the step's table failed".into())?;
    ensure(provider.invocations("wf-2") == 1, || "the provider retries no failed step".into())
}

/// A stop on a running chunk step's run closes it `canceled` through the failure path; the chunk
/// returns to pending, and the provider's replay resumes it from its recorded pages.
pub fn stop(bin: &Path, make: &Make) -> Result<(), String> {
    let vendor = Vendor::start();
    let p = Project::seeded(bin, &vendor);
    let mut provider = make(&p.target);
    vendor.hang(3);
    let mut stopped = Ok(String::new());
    let canceled = provider.step_during("wf-1", &Unit::Chunk(0), T0, &|| vendor.hung(), &mut || {
        stopped = p.ok(&["run", "cancel", "wf-1.seed-0", "--project", PROJECT, "--reason", "operator stop"]);
        vendor.release();
    });
    let stopped = stopped?;
    ensure(stopped.contains("wf-1.seed-0: stop requested"), || format!("the stop marks the step's run: {stopped}"))?;
    ensure(canceled.is_err(), || format!("a stopped step returns its failure: {canceled:?}"))?;
    ensure(p.status("wf-1.seed-0")? == "canceled", || "the stopped attempt closes canceled".into())?;
    vendor.take();

    let pointer = provider.step("wf-1", &Unit::Chunk(0), T0)?;
    ensure(pointer == "orders_items chunk 0: commit wf-1.replay-2.seed-0", || format!("the replay resumes the chunk: {pointer}"))?;
    let sent = vendor.take();
    ensure(!sent.iter().any(|t| t.ends_with("page=1") || t.ends_with("page=2")), || format!("no recorded page is sent again: {sent:?}"))?;
    ensure(p.show("wf-1.replay-2.seed-0")?.is_some_and(|row| row["rows"] == 8), || "the resumed chunk lands all eight rows".into())
}

/// A `call` delegate the job's step reaches: each envelope recorded, every answer `ok` and signed,
/// and an item whose entity is `hold` held unanswered while `holding` is set.
struct Delegate {
    server: Server,
    envelopes: Arc<Mutex<Vec<Value>>>,
    holding: Arc<AtomicBool>,
    held: Arc<AtomicBool>,
}

impl Delegate {
    fn start(hold: &'static str) -> Delegate {
        let (envelopes, holding, held): (Arc<Mutex<Vec<Value>>>, Arc<AtomicBool>, Arc<AtomicBool>) = Default::default();
        let (seen, waiting, arrived) = (envelopes.clone(), holding.clone(), held.clone());
        let server = Server::start(move |request: &Request| {
            let envelope: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
            let signed = request_signed(KEY, request);
            seen.lock().unwrap().push(json!({ "signed": signed, "envelope": envelope }));
            let row = &envelope["row"];
            if row["entity_id"] == hold && waiting.load(Ordering::SeqCst) {
                arrived.store(true, Ordering::SeqCst);
                while waiting.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            let body = json!({ "outcome": "ok", "rows": { "screen_result": [{ "entity_id": row["entity_id"], "pass_id": row["pass_id"], "proceed": true }] } }).to_string();
            signed_answer(KEY, envelope["idempotency_key"].as_str().unwrap_or_default(), &body)
        });
        Delegate { server, envelopes, holding, held }
    }

    /// Every envelope since the last call, each as `{signed, envelope}`.
    fn take(&self) -> Vec<Value> {
        std::mem::take(&mut *self.envelopes.lock().unwrap())
    }
}

/// A project whose store-driven job `stage-screen` runs two isolated items on `delegate`.
fn delegated(bin: &Path, delegate: &Delegate) -> Result<Project, String> {
    let mut p = Project::new(
        bin,
        "",
        &format!(
            "authoring_posture = \"per_request\"\n\n\
             [control.delegates.stages]\nurl = \"{}\"\nmode = \"call\"\ntimeout = \"30s\"\n\n\
             [[job]]\nname = \"stage-screen\"\nkind = \"store-driven\"\nbody = {{ delegate = \"stages\" }}\n\
             statement = \"SELECT entity_id, pass_id FROM due_items ORDER BY pass_id\"\n\
             as_of = \"2030-01-01T00:00:10Z\"\nmax_in_flight = 1\ntables = [\"screen_result\"]\n\
             isolated_items = {{ logical_key = \"pass_id\", entity_key = \"entity_id\", entity_namespace = \"entity\", max_attempts = 3 }}\n",
            delegate.server.url(ROUTE)
        ),
    );
    p.repo.write(".contextful/issuance.toml", &format!("default_audience = \"{AUDIENCE}\"\nmax_lifetime_secs = 3600\n"));
    p.repo.write("due.jsonl", "{\"entity_id\":\"e-1\",\"pass_id\":\"p-1\"}\n{\"entity_id\":\"e-2\",\"pass_id\":\"p-2\"}\n");
    p.ok(&["context", "land", "due_items", "--project", PROJECT, "--rows", "due.jsonl", "--run-id", "load-1", "--site-id", SITE, "--now", T0])?;
    let public = p.ok(&["token", "keygen", "--out", ".contextful/issuer.seed"])?;
    let token = p.ok(&["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://stages", "--zone", "on-prem:hq", "--table", "due_items", "--ttl", "600"])?;
    p.target.env = vec![("CONTEXTFUL_TOKEN".into(), token), ("CONTEXTFUL_WORKER_KEY".into(), String::from_utf8_lossy(KEY).into_owned())];
    p.target.job_args = vec!["--public-key".into(), public, "--audience".into(), AUDIENCE.into()];
    Ok(p)
}

/// A job step's `call` delegate runs inside the step under the engine's entry key: a step crashed
/// while one call is unanswered replays the answered call from the engine's record and re-sends
/// the other under its identical idempotency key; a replay of the completed step sends nothing.
pub fn delegate_inside(bin: &Path, make: &Make) -> Result<(), String> {
    let delegate = Delegate::start("e-2");
    let p = delegated(bin, &delegate)?;
    let mut provider = make(&p.target);
    let job = Unit::Job("stage-screen".into());

    // One item in flight: the second item's call arrives after the first item's answer.
    delegate.holding.store(true, Ordering::SeqCst);
    provider.step_killed("wf-1", &job, T0, &|| delegate.held.load(Ordering::SeqCst));
    delegate.holding.store(false, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    let crashed = delegate.take();
    ensure(crashed.len() == 2, || format!("the crashed step sent one call per item: {crashed:?}"))?;
    ensure(crashed.iter().all(|e| e["signed"] == true), || "each call is signed under the deployment key".into())?;
    let unanswered = crashed.iter().find(|e| e["envelope"]["row"]["entity_id"] == "e-2").map(|e| e["envelope"].clone()).ok_or("the held item was sent")?;
    let label = unanswered["step"].as_str().unwrap_or_default();
    ensure(label.starts_with("row/") && label.ends_with("/delegate/1"), || format!("the call is a journaled step of the engine: {unanswered}"))?;

    let pointer = provider.step("wf-1", &job, T1)?;
    ensure(pointer.contains("2 successful items, 0 failed"), || format!("the replayed step completes both items: {pointer}"))?;
    let replayed = delegate.take();
    ensure(replayed.len() == 1, || format!("the replay sends only the unanswered call: {replayed:?}"))?;
    let resent = &replayed[0]["envelope"];
    ensure(resent["row"]["entity_id"] == "e-2", || format!("the re-sent call is the held item's: {resent}"))?;
    for field in ["idempotency_key", "execution_id", "step", "input_hash"] {
        ensure(resent[field] == unanswered[field], || format!("the re-sent call carries the crashed call's `{field}`: {resent} against {unanswered}"))?;
    }
    let landed = p.select("SELECT entity_id, pass_id FROM screen_result ORDER BY pass_id")?;
    ensure(landed == [["e-1", "p-1"], ["e-2", "p-2"]], || format!("both items land once: {landed:?}"))?;

    provider.step("wf-1", &job, T1)?;
    let sent = delegate.take();
    ensure(sent.is_empty(), || format!("a completed step's replay sends no call: {sent:?}"))?;
    ensure(p.select("SELECT entity_id FROM screen_result")?.len() == 2, || "and lands nothing new".into())?;
    ensure(provider.invocations("wf-1") == 3, || "the provider invoked the step three times".into())
}
