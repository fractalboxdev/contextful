//! `assurance.deploy-apps`: the one GitHub Actions workflow and the command it runs.
//! `contextful-ci deploy workflow` holds `.github/workflows` to the fixed-shape deploy
//! workflow; `contextful-ci deploy cloudflare --app <app>` ships one Cloudflare app with
//! the two variables an operator also sets locally, `CLOUDFLARE_API_TOKEN` and
//! `CLOUDFLARE_ACCOUNT_ID`.

use crate::{refuse, release};
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// The sole workflow file, relative to `.github/workflows`.
const WORKFLOW: &str = "deploy-cloudflare.yml";
/// `assurance.deploy-apps.job-timeout`.
const JOB_TIMEOUT_MIN: u64 = 90;
const RUNNER: &str = "ubuntu-24.04";
const ACTIONS: [&str; 2] = ["actions/checkout", "actions/setup-node"];
const SECRETS: [&str; 2] = ["CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ACCOUNT_ID"];
/// `assurance.deploy-apps.one-command`.
const COMMAND: &str = "cargo run --locked -q -p contextful-ci -- deploy cloudflare --app \"$APP\"";

/// The app whose lockfile pins the wrangler every deploy runs.
const WRANGLER_APP: &str = "apps/cloudflare-execution";
/// The release the execution app's container carries (`assurance.deploy-apps.container-image`).
const IMAGE_PROFILE: &str = "contextful-full";
const IMAGE_TARGET: &str = "x86_64-unknown-linux-musl";
/// Where the containerized release build compiles and packages, relative to the root.
const DEPLOY_TMP: &str = ".tmp/deploy";
/// The rendered config beside an app's template; `.gitignore` holds it untracked.
const RENDERED: &str = "wrangler.deploy.jsonc";
/// How long the health check waits for a first answer.
const HEALTH_WAIT: Duration = Duration::from_secs(120);
const HEALTH_INTERVAL: Duration = Duration::from_secs(5);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(10);

/// A Cloudflare app the deploy command ships, named by its directory under `apps/`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum App {
    #[value(name = "cloudflare-execution")]
    Execution,
    Gateway,
    Console,
}

impl App {
    fn dir(self) -> String {
        format!("apps/{}", self.name())
    }

    fn name(self) -> String {
        self.to_possible_value().expect("every app has a name").get_name().to_string()
    }

    /// `assurance.deploy-apps.expected-status`.
    fn expected(self) -> &'static [u16] {
        match self {
            App::Execution => &[404],
            App::Gateway => &[404, 503],
            App::Console => &[200, 302],
        }
    }
}

fn names() -> Vec<String> {
    App::value_variants().iter().map(|a| a.name()).collect()
}

/// `assurance.deploy-apps.workflow-refused`: `.github/workflows` holds the deploy workflow
/// alone, in its fixed shape.
pub fn workflow(root: &Path) -> Result<()> {
    let dir = root.join(".github/workflows");
    if !dir.is_dir() {
        return Ok(());
    }
    let mut files: Vec<String> = std::fs::read_dir(&dir)?.map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned())).collect::<std::io::Result<_>>()?;
    files.sort();
    if let Some(other) = files.iter().find(|f| *f != WORKFLOW) {
        return Err(refused(other, format!("the only workflow is {WORKFLOW}")));
    }
    for file in &files {
        let text = std::fs::read_to_string(dir.join(file))?;
        shape(&text).map_err(|breach| refused(file, breach))?;
    }
    Ok(())
}

fn refused(file: &str, breach: String) -> anyhow::Error {
    refuse("DeployWorkflowRefused", format!(".github/workflows/{file}: {breach}"))
}

/// Refuse a key of `value` outside `allowed`, or a non-object.
fn keys(value: &Value, what: &str, allowed: &[&str]) -> Result<(), String> {
    let object = value.as_object().ok_or(format!("{what} is not a mapping"))?;
    match object.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(key) => Err(format!("{what} carries `{key}`; it admits {}", allowed.join(", "))),
        None => Ok(()),
    }
}

/// The breach of the fixed deploy-workflow shape `text` commits, if any.
fn shape(text: &str) -> Result<(), String> {
    // JSON is a YAML subset: the fixed shape needs no second parser.
    let flow: Value = serde_json::from_str(text).map_err(|e| format!("not written in YAML's JSON subset ({e})"))?;
    keys(&flow, "the workflow", &["name", "run-name", "on", "permissions", "concurrency", "jobs"])?;
    trigger(&flow["on"])?;
    if flow["permissions"] != json!({"contents": "read"}) {
        return Err("permissions grant more or other than `contents: read`".into());
    }
    if let Some(c) = flow.get("concurrency") {
        keys(c, "concurrency", &["group", "cancel-in-progress"])?;
    }
    let jobs = flow["jobs"].as_object().ok_or("no jobs")?;
    let [(name, job)] = jobs.iter().collect::<Vec<_>>()[..] else {
        return Err(format!("{} jobs, where one deploys", jobs.len()));
    };
    let what = format!("job `{name}`");
    keys(job, &what, &["name", "runs-on", "timeout-minutes", "steps"])?;
    if job["runs-on"] != RUNNER {
        return Err(format!("{what} runs on {}, not the fixed {RUNNER} image", job["runs-on"]));
    }
    match job["timeout-minutes"].as_u64() {
        Some(n) if (1..=JOB_TIMEOUT_MIN).contains(&n) => {}
        _ => return Err(format!("{what} sets no `timeout-minutes` from 1 to {JOB_TIMEOUT_MIN}")),
    }
    let steps = job["steps"].as_array().ok_or(format!("{what} has no steps"))?;
    let mut runs = 0;
    for (i, step) in steps.iter().enumerate() {
        let what = format!("step {i}");
        keys(step, &what, &["name", "uses", "with", "run", "env"])?;
        match (step.get("uses"), step.get("run")) {
            (Some(uses), None) => action(&what, uses, step)?,
            (None, Some(run)) => {
                runs += 1;
                if run != COMMAND {
                    return Err(format!("{what} runs {run}, not `{COMMAND}`"));
                }
                let env = json!({
                    "APP": "${{ inputs.app }}",
                    "CLOUDFLARE_API_TOKEN": "${{ secrets.CLOUDFLARE_API_TOKEN }}",
                    "CLOUDFLARE_ACCOUNT_ID": "${{ secrets.CLOUDFLARE_ACCOUNT_ID }}",
                });
                if step["env"] != env {
                    return Err(format!("{what}'s environment is other than the app and the two Cloudflare secrets"));
                }
            }
            _ => return Err(format!("{what} holds neither or both of `uses` and `run`")),
        }
    }
    if runs != 1 {
        return Err(format!("{runs} run steps, where one runs the deploy command"));
    }
    literals(text)
}

/// `assurance.deploy-apps.workflow-trigger`: `workflow_dispatch` alone, a required `app`
/// choice of every app and an optional `ref`.
fn trigger(on: &Value) -> Result<(), String> {
    keys(on, "`on`", &["workflow_dispatch"])?;
    let inputs = &on["workflow_dispatch"]["inputs"];
    keys(&on["workflow_dispatch"], "`workflow_dispatch`", &["inputs"])?;
    keys(inputs, "the inputs", &["app", "ref"])?;
    let app = &inputs["app"];
    keys(app, "input `app`", &["description", "type", "required", "options"])?;
    if app["type"] != "choice" || app["required"] != true || app["options"] != json!(names()) {
        return Err(format!("input `app` is not a required choice of {}", names().join(", ")));
    }
    if let Some(r) = inputs.get("ref") {
        keys(r, "input `ref`", &["description", "type", "required", "default"])?;
        if r["type"] != "string" || r.get("required").is_some_and(|v| v != false) {
            return Err("input `ref` is not an optional string".into());
        }
    }
    Ok(())
}

/// `assurance.deploy-apps.workflow-permissions`: a trusted action pinned to a 40-hex commit;
/// a checkout persisting no credentials.
fn action(what: &str, uses: &Value, step: &Value) -> Result<(), String> {
    let uses = uses.as_str().ok_or(format!("{what}'s `uses` is not a string"))?;
    let (name, rev) = uses.split_once('@').ok_or(format!("{what} uses {uses} at no commit"))?;
    if !ACTIONS.contains(&name) {
        return Err(format!("{what} uses {name}, outside {}", ACTIONS.join(", ")));
    }
    if rev.len() != 40 || !rev.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return Err(format!("{what} pins {name} to `{rev}`, not a 40-hex commit"));
    }
    if step.get("env").is_some() {
        return Err(format!("{what} gives {name} an environment"));
    }
    let with = step.get("with").cloned().unwrap_or(json!({}));
    if name == "actions/checkout" {
        keys(&with, &format!("{what}'s `with`"), &["ref", "persist-credentials", "fetch-depth"])?;
        if with["persist-credentials"] != false {
            return Err(format!("{what} checks out persisting credentials"));
        }
    } else {
        keys(&with, &format!("{what}'s `with`"), &["node-version"])?;
    }
    Ok(())
}

/// `assurance.deploy-apps.workflow-credentials`: the two Cloudflare secrets alone, and no
/// URL, hostname or run of 32 hex digits outside a pinned commit.
fn literals(text: &str) -> Result<(), String> {
    for (at, _) in text.match_indices("secrets.") {
        let name: String = text[at + "secrets.".len()..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !SECRETS.contains(&name.as_str()) {
            return Err(format!("reads secret `{name}`; the workflow reads {} alone", SECRETS.join(" and ")));
        }
    }
    if text.contains("://") || text.contains("workers.dev") {
        return Err("carries a URL or hostname".into());
    }
    for (start, run) in hex_runs(text) {
        let digits = run.bytes().filter(u8::is_ascii_hexdigit).count();
        let pinned = digits == 40 && run.len() == 40 && text[..start].ends_with('@');
        if digits >= 32 && !pinned {
            return Err(format!("carries `{run}`, an identifier outside a pinned commit"));
        }
    }
    Ok(())
}

/// Each maximal run of hex digits and dashes in `text`, with its byte offset.
fn hex_runs(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, b) in text.bytes().enumerate().chain(std::iter::once((text.len(), b' '))) {
        let part = b.is_ascii_hexdigit() || b == b'-';
        match (part, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push((s, &text[s..i]));
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// `text` with each URL, `workers.dev` hostname and run of 32 or more hex digits replaced
/// (`assurance.deploy-apps.quiet-output`).
pub fn redact(text: &str) -> String {
    let words: Vec<String> = text
        .split_inclusive(char::is_whitespace)
        .map(|word| {
            let (body, space) = word.split_at(word.trim_end().len());
            if body.contains("://") || body.contains("workers.dev") {
                return format!("<url>{space}");
            }
            let mut out = body.to_string();
            for (_, run) in hex_runs(body) {
                if run.bytes().filter(u8::is_ascii_hexdigit).count() >= 32 {
                    out = out.replace(run, "<id>");
                }
            }
            out + space
        })
        .collect();
    words.concat()
}

/// `assurance.deploy-apps.credentials-absent`: both Cloudflare variables set and non-empty.
fn credentials() -> Result<()> {
    for var in SECRETS {
        if std::env::var(var).map_or(true, |v| v.trim().is_empty()) {
            return Err(refuse("DeployCredentialsAbsent", format!("{var} is unset or empty; export it, or set the repository secret the workflow reads")));
        }
    }
    Ok(())
}

/// One captured step: run `cmd` in `dir`, print `step` alone, and on failure print its
/// output redacted.
fn step(step: &str, dir: &Path, program: &Path, args: &[&str]) -> Result<Output> {
    println!("deploy: {step}");
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0")
        .env("WRANGLER_SEND_METRICS", "false")
        .output()
        .with_context(|| format!("running {} for {step}", program.display()))?;
    if !out.status.success() {
        eprint!("{}", redact(&String::from_utf8_lossy(&out.stdout)));
        eprint!("{}", redact(&String::from_utf8_lossy(&out.stderr)));
        bail!("{step} exited {}", out.status.code().map_or("by signal".into(), |c| c.to_string()));
    }
    Ok(out)
}

struct Wrangler {
    bin: PathBuf,
}

impl Wrangler {
    fn run(&self, what: &str, dir: &Path, args: &[&str]) -> Result<Output> {
        step(what, dir, &self.bin, args)
    }

    /// Whether the command exits zero; its output is discarded.
    fn succeeds(&self, dir: &Path, args: &[&str]) -> Result<bool> {
        Ok(Command::new(&self.bin).args(args).current_dir(dir).env("WRANGLER_SEND_METRICS", "false").output()?.status.success())
    }

    /// The identifier of D1 database `name`, created when absent.
    fn d1(&self, dir: &Path, name: &str) -> Result<String> {
        let find = || -> Result<Option<String>> {
            let out = self.run(&format!("find D1 database {name}"), dir, &["d1", "list", "--json"])?;
            let list: Value = serde_json::from_slice(&out.stdout).context("`wrangler d1 list --json` printed no JSON array")?;
            Ok(list.as_array().into_iter().flatten().find(|d| d["name"] == name).and_then(|d| d["uuid"].as_str()).map(str::to_string))
        };
        if let Some(id) = find()? {
            return Ok(id);
        }
        self.run(&format!("create D1 database {name}"), dir, &["d1", "create", name])?;
        find()?.with_context(|| format!("D1 database {name} is absent after its creation"))
    }

    /// Create R2 bucket `name` when absent.
    fn r2(&self, dir: &Path, name: &str) -> Result<()> {
        println!("deploy: find R2 bucket {name}");
        if !self.succeeds(dir, &["r2", "bucket", "info", name])? {
            self.run(&format!("create R2 bucket {name}"), dir, &["r2", "bucket", "create", name])?;
        }
        Ok(())
    }
}

/// The name a placeholder resource takes: the Worker's name and the binding's slug, the
/// binding's leading word dropped when it repeats the Worker's last.
fn resource_name(worker: &str, binding: &str, committed: &str) -> String {
    if !committed.starts_with("replace-with-") {
        return committed.to_string();
    }
    let slug = binding.to_ascii_lowercase().replace('_', "-");
    let last = worker.rsplit('-').next().unwrap_or(worker);
    let slug = slug.strip_prefix(&format!("{last}-")).unwrap_or(&slug);
    format!("{worker}-{slug}")
}

/// The builder image the root Dockerfile pins: its `AS build` stage's base.
fn builder_image(root: &Path) -> Result<String> {
    let text = std::fs::read_to_string(root.join("Dockerfile")).context("reading Dockerfile")?;
    text.lines()
        .filter(|l| l.starts_with("FROM ") && l.trim_end().ends_with(" AS build"))
        .find_map(|l| l.split_whitespace().skip(1).find(|w| !w.starts_with("--")).map(str::to_string))
        .filter(|image| image.contains("@sha256:"))
        .context("the Dockerfile pins no `AS build` stage by digest")
}

/// `contextful-ci deploy cloudflare --app <app>`.
pub fn cloudflare(root: &Path, app: App) -> Result<()> {
    credentials()?;
    let dir = root.join(app.dir());
    let tools = root.join(WRANGLER_APP);
    step("install wrangler", &tools, Path::new("npm"), &["ci", "--no-audit", "--no-fund"])?;
    let wrangler = Wrangler { bin: tools.join("node_modules/.bin/wrangler") };
    let config = match app {
        App::Execution => render(root, &dir, &wrangler)?,
        App::Console => {
            step("install console", &dir, Path::new("corepack"), &["pnpm", "install", "--frozen-lockfile"])?;
            step("build console", &dir, Path::new("corepack"), &["pnpm", "run", "build:preview"])?;
            "wrangler.jsonc".to_string()
        }
        App::Gateway => "wrangler.jsonc".to_string(),
    };
    let out = wrangler.run(&format!("deploy {}", app.name()), &dir, &["deploy", "--config", &config])?;
    let printed = String::from_utf8_lossy(&out.stdout);
    let url = printed
        .split_whitespace()
        .find(|w| w.starts_with("https://") || w.starts_with("http://"))
        .map(|w| w.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()).to_string())
        .ok_or_else(|| refuse("DeployUnhealthy", format!("wrangler reported no URL for {}", app.name())))?;
    health(app, &url)
}

/// `assurance.deploy-apps.resources` and `container-image`: ensure the bound resources,
/// package the release image context, and write `wrangler.deploy.jsonc`.
fn render(root: &Path, dir: &Path, wrangler: &Wrangler) -> Result<String> {
    let template = dir.join("wrangler.example.jsonc");
    let mut config: Value = serde_json::from_str(&std::fs::read_to_string(&template).with_context(|| format!("reading {}", template.display()))?)
        .with_context(|| format!("{} is not JSON", template.display()))?;
    let worker = config["name"].as_str().context("the template names no Worker")?.to_string();
    for db in config["d1_databases"].as_array_mut().into_iter().flatten() {
        let name = resource_name(&worker, db["binding"].as_str().unwrap_or("db"), db["database_name"].as_str().unwrap_or("replace-with-"));
        db["database_id"] = json!(wrangler.d1(dir, &name)?);
        db["database_name"] = json!(name);
    }
    for bucket in config["r2_buckets"].as_array_mut().into_iter().flatten() {
        let name = resource_name(&worker, bucket["binding"].as_str().unwrap_or("bucket"), bucket["bucket_name"].as_str().unwrap_or("replace-with-"));
        wrangler.r2(dir, &name)?;
        bucket["bucket_name"] = json!(name);
    }
    // The conformance route answers its token alone; every other route answers 404.
    config["workers_dev"] = json!(true);
    if let Some(context) = config["containers"].as_array().and_then(|c| c.first()).and_then(|c| c["image_build_context"].as_str()) {
        image(root, dir, &dir.join(context))?;
    }
    std::fs::write(dir.join(RENDERED), serde_json::to_string_pretty(&config)? + "\n")?;
    Ok(RENDERED.to_string())
}

/// Package the `contextful-full` musl release inside the Dockerfile's builder, then write
/// the image context `prepare-image` derives from that archive.
fn image(root: &Path, dir: &Path, context: &Path) -> Result<()> {
    let tmp = root.join(DEPLOY_TMP);
    std::fs::create_dir_all(&tmp)?;
    let builder = builder_image(root)?;
    let script = format!(
        "apk add --no-cache build-base git && git config --global --add safe.directory /src && \
         cargo run --locked -q -p contextful-ci -- release --profile {IMAGE_PROFILE} --target {IMAGE_TARGET} --target-dir {DEPLOY_TMP}/target --out {DEPLOY_TMP}/dist"
    );
    let mount = format!("{}:/src", root.display());
    let target_dir = format!("CARGO_TARGET_DIR=/src/{DEPLOY_TMP}/target");
    step(
        "package the release in the builder image",
        root,
        Path::new("docker"),
        &["run", "--rm", "--platform", "linux/amd64", "-v", &mount, "-w", "/src", "-e", &target_dir, &builder, "sh", "-c", &script],
    )?;
    let dist = tmp.join("dist");
    let stem = release::stem(IMAGE_PROFILE, &release::version(root)?, IMAGE_TARGET);
    let path = |suffix: &str| dist.join(format!("{stem}{suffix}")).display().to_string();
    if context.exists() {
        std::fs::remove_dir_all(context).with_context(|| format!("clearing {}", context.display()))?;
    }
    step(
        "prepare the image context",
        dir,
        Path::new("npm"),
        &["run", "prepare-image", "--", &path(".tar.gz"), &path(".release.json"), &path(".cloudflare-d1.compatibility.json"), &context.display().to_string()],
    )?;
    Ok(())
}

/// `assurance.deploy-apps.health`: the first answer to an anonymous `GET /`, judged against
/// the app's expected statuses; the URL is never printed.
fn health(app: App, url: &str) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder().max_redirects(0).http_status_as_error(false).timeout_global(Some(HEALTH_TIMEOUT)).build().into();
    let start = Instant::now();
    let status = loop {
        match agent.get(format!("{}/", url.trim_end_matches('/'))).call() {
            Ok(response) => break response.status().as_u16(),
            Err(_) if start.elapsed() < HEALTH_WAIT => std::thread::sleep(HEALTH_INTERVAL),
            Err(e) => return Err(refuse("DeployUnhealthy", format!("{} gave no answer within {}s ({})", app.name(), HEALTH_WAIT.as_secs(), redact(&e.to_string())))),
        }
    };
    println!("deploy: health {status}");
    if !app.expected().contains(&status) {
        let expected: Vec<String> = app.expected().iter().map(u16::to_string).collect();
        return Err(refuse("DeployUnhealthy", format!("{} answered {status}, where it answers {}", app.name(), expected.join(" or "))));
    }
    Ok(())
}
