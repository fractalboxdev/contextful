//! `assurance.deploy-apps`: the one Actions workflow keeps a fixed dispatch, permission and
//! credential shape, and `contextful-ci deploy cloudflare` ships one app with the two
//! Cloudflare variables an operator also sets locally.

use crate::{repo_root, stderr, Repo};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};

const WORKFLOW: &str = ".github/workflows/deploy-cloudflare.yml";
const CHECKOUT: &str = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1";
const SETUP_NODE: &str = "actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1";
const APPS: [&str; 3] = ["cloudflare-execution", "gateway", "console"];
const D1_UUID: &str = "aaaaaaaa-1111-2222-3333-444444444444";

/// A deploy workflow fixture, deliberately independent of the committed file.
fn valid() -> Value {
    json!({
        "name": "Deploy a Cloudflare app",
        "on": {"workflow_dispatch": {"inputs": {
            "app": {"description": "The app to deploy", "type": "choice", "required": true, "options": APPS},
            "ref": {"description": "The revision to deploy", "type": "string", "required": false, "default": ""}
        }}},
        "permissions": {"contents": "read"},
        "concurrency": {"group": "deploy-cloudflare-${{ inputs.app }}", "cancel-in-progress": false},
        "jobs": {"deploy": {"runs-on": "ubuntu-24.04", "timeout-minutes": 90, "steps": [
            {"uses": CHECKOUT, "with": {"ref": "${{ inputs.ref || github.sha }}", "persist-credentials": false}},
            {"uses": SETUP_NODE, "with": {"node-version": "22"}},
            {"name": "Deploy", "run": "cargo run --locked -q -p contextful-ci -- deploy cloudflare --app \"$APP\"", "env": {
                "APP": "${{ inputs.app }}",
                "CLOUDFLARE_API_TOKEN": "${{ secrets.CLOUDFLARE_API_TOKEN }}",
                "CLOUDFLARE_ACCOUNT_ID": "${{ secrets.CLOUDFLARE_ACCOUNT_ID }}"
            }}
        ]}}
    })
}

/// The deploy workflow's step `i`.
fn step(flow: &mut Value, i: usize) -> &mut Value {
    &mut flow["jobs"]["deploy"]["steps"][i]
}

/// `deploy workflow` checks the tree's workflows alone, as the schema stage does.
fn check(r: &Repo) -> Output {
    r.run_ci(&["deploy", "workflow"])
}

/// A file under `.github/workflows` other than the deploy workflow, or a deploy workflow breaching its trigger, permission, credential, timeout or command clauses, raises `DeployWorkflowRefused` in the schema stage.
// spec: assurance.deploy-apps.workflow-refused@67f93cc7
// spec: assurance.deploy-apps.workflow-trigger@5a91ef21
// spec: assurance.deploy-apps.workflow-permissions@b907fd8b
// spec: assurance.deploy-apps.workflow-credentials@219366b7
// spec: assurance.deploy-apps.job-timeout@cd70d414
#[test]
fn the_deploy_workflow_refuses_broad_triggers_write_permissions_unpinned_actions_and_literal_credentials() {
    let r = Repo::init();
    let write = |flow: &Value| r.write(WORKFLOW, &serde_json::to_string_pretty(flow).unwrap());
    write(&valid());
    let o = check(&r);
    assert!(o.status.success(), "the fixed-shape deploy workflow is refused: {}", stderr(&o));

    type Mutation = Box<dyn Fn(&mut Value)>;
    let mutations: Vec<(&str, Mutation)> = vec![
        ("push trigger", Box::new(|f| f["on"]["push"] = json!({}))),
        ("pull_request_target trigger", Box::new(|f| f["on"] = json!({"pull_request_target": {}}))),
        ("schedule trigger", Box::new(|f| f["on"]["schedule"] = json!([{"cron": "0 0 * * *"}]))),
        ("contents write", Box::new(|f| f["permissions"]["contents"] = json!("write"))),
        ("id-token write", Box::new(|f| f["permissions"]["id-token"] = json!("write"))),
        ("write-all", Box::new(|f| f["permissions"] = json!("write-all"))),
        ("job permissions", Box::new(|f| f["jobs"]["deploy"]["permissions"] = json!({"contents": "write"}))),
        ("unpinned action", Box::new(|f| step(f, 0)["uses"] = json!("actions/checkout@v7"))),
        ("short commit", Box::new(|f| step(f, 0)["uses"] = json!("actions/checkout@3d3c42e"))),
        ("untrusted action", Box::new(|f| step(f, 1)["uses"] = json!("someone/deploy@3d3c42e5aac5ba805825da76410c181273ba90b1"))),
        ("persisted credentials", Box::new(|f| step(f, 0)["with"]["persist-credentials"] = json!(true))),
        ("literal account id", Box::new(|f| step(f, 2)["env"]["CLOUDFLARE_ACCOUNT_ID"] = json!("0123456789abcdef0123456789abcdef"))),
        ("account id from a variable", Box::new(|f| step(f, 2)["env"]["CLOUDFLARE_ACCOUNT_ID"] = json!("${{ vars.CLOUDFLARE_ACCOUNT_ID }}"))),
        ("third secret", Box::new(|f| step(f, 2)["env"]["EXTRA"] = json!("${{ secrets.OTHER_TOKEN }}"))),
        ("hostname", Box::new(|f| step(f, 2)["env"]["HEALTH"] = json!("https://contextful-gateway.example.workers.dev"))),
        ("arbitrary command", Box::new(|f| step(f, 2)["run"] = json!("npx wrangler deploy"))),
        ("second run step", Box::new(|f| f["jobs"]["deploy"]["steps"].as_array_mut().unwrap().push(json!({"run": "echo hi"})))),
        ("timeout over 90 min", Box::new(|f| f["jobs"]["deploy"]["timeout-minutes"] = json!(91))),
        ("no timeout", Box::new(|f| { f["jobs"]["deploy"].as_object_mut().unwrap().remove("timeout-minutes"); })),
        ("moving runner image", Box::new(|f| f["jobs"]["deploy"]["runs-on"] = json!("ubuntu-latest"))),
        ("second job", Box::new(|f| f["jobs"]["other"] = f["jobs"]["deploy"].clone())),
        ("app choice short of the apps", Box::new(|f| f["on"]["workflow_dispatch"]["inputs"]["app"]["options"] = json!(["gateway"]))),
        ("free-form app", Box::new(|f| f["on"]["workflow_dispatch"]["inputs"]["app"]["type"] = json!("string"))),
    ];
    for (name, mutate) in &mutations {
        let mut flow = valid();
        mutate(&mut flow);
        write(&flow);
        let o = check(&r);
        assert!(!o.status.success(), "a deploy workflow with {name} passes");
        assert!(stderr(&o).contains("DeployWorkflowRefused") && stderr(&o).contains("deploy-cloudflare.yml"), "{name}: {}", stderr(&o));
    }

    write(&valid());
    r.write(WORKFLOW, "on: workflow_dispatch\npermissions:\n  contents: read\n");
    assert!(stderr(&check(&r)).contains("DeployWorkflowRefused"), "a workflow outside YAML's JSON subset passes");
    write(&valid());
    r.write(".github/workflows/ci.yml", &serde_json::to_string(&json!({"on": {"push": {}}})).unwrap());
    let o = check(&r);
    assert!(stderr(&o).contains("DeployWorkflowRefused") && stderr(&o).contains("ci.yml"), "a second workflow passes: {}", stderr(&o));

    // The schema stage runs the same check.
    r.commit("a second workflow");
    let o = r.gate(&["--stage", "schema"]);
    assert!(!o.status.success() && stderr(&o).contains("DeployWorkflowRefused"), "{}", stderr(&o));
}

/// The committed workflow passes the check, and its one `run` step is the command an operator runs locally.
// spec: assurance.deploy-apps.one-command@2e077ea7
#[test]
fn the_committed_workflow_passes_and_runs_the_operator_command() {
    let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args(["deploy", "workflow"]).current_dir(repo_root()).output().unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let flow: Value = serde_json::from_str(&std::fs::read_to_string(repo_root().join(WORKFLOW)).unwrap()).unwrap();
    assert_eq!(flow["on"]["workflow_dispatch"]["inputs"]["app"]["options"], json!(APPS));
    let runs: Vec<&str> = flow["jobs"].as_object().unwrap().values().flat_map(|job| job["steps"].as_array().unwrap()).filter_map(|s| s["run"].as_str()).collect();
    assert_eq!(runs, ["cargo run --locked -q -p contextful-ci -- deploy cloudflare --app \"$APP\""]);
    // `--app` takes exactly the workflow's choices.
    let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args(["deploy", "cloudflare", "--app", "docs"]).current_dir(repo_root()).output().unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    for app in APPS {
        assert!(stderr(&o).contains(app), "`--app` omits {app}: {}", stderr(&o));
    }
}

/// A directory of fake `npm`, `docker` and `corepack` on `PATH`, each appending its
/// arguments to `log`.
fn fakes(dir: &Path, log: &Path) {
    for tool in ["npm", "docker", "corepack"] {
        let path = dir.join(tool);
        std::fs::write(&path, format!("#!/bin/sh\necho \"{tool} $*\" >> '{}'\n", log.display())).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
}

/// The wrangler the deploy command runs, faked: it logs each call, keeps one D1 database
/// once created, misses every R2 bucket, and deploys to `url`, or fails naming an
/// identifier and a URL when `fail` holds.
fn fake_wrangler(r: &Repo, log: &Path, url: &str, fail: bool) {
    let state = r.root.join(".fake-d1");
    let deploy = if fail {
        format!("echo \"deploy failed at {url} for account 0123456789abcdef0123456789abcdef\" >&2; exit 1")
    } else {
        format!("echo 'Uploaded contextful'; echo '  {url}'; echo 'Current Version ID: bbbbbbbb-1111-2222-3333-444444444444'")
    };
    let script = format!(
        "#!/bin/sh\necho \"wrangler $*\" >> '{log}'\ncase \"$1 $2\" in\n\
         'd1 list') if [ -f '{state}' ]; then echo '[{{\"uuid\":\"{D1_UUID}\",\"name\":\"contextful-execution-db\"}}]'; else echo '[]'; fi ;;\n\
         'd1 create') touch '{state}'; echo 'database_id = \"{D1_UUID}\"' ;;\n\
         'r2 bucket') [ \"$3\" = create ] || exit 1 ;;\n\
         deploy*) {deploy} ;;\n\
         esac\n",
        log = log.display(),
        state = state.display(),
    );
    let path = r.root.join("apps/cloudflare-execution/node_modules/.bin/wrangler");
    r.write("apps/cloudflare-execution/node_modules/.bin/wrangler", &script);
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// A local server answering every request with `status`, at most `n` times.
fn answering(status: u16, n: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(n).flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    format!("http://{addr}")
}

/// A fixture repository holding the apps' wrangler configs and the Dockerfile's builder line.
fn apps_repo() -> Repo {
    let r = Repo::init();
    r.write("Cargo.toml", "[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"0.17.1\"\n");
    r.write("Dockerfile", "ARG PLATFORM=linux/amd64\nFROM --platform=$PLATFORM rust:1.97-alpine@sha256:3c38f3f82c2f3d73da3b38e18d279393a04cb43ddded0e35088a8c3324d40900 AS build\n");
    r.write("apps/gateway/wrangler.jsonc", "{\n  \"name\": \"contextful-gateway\",\n  \"main\": \"src/index.ts\"\n}\n");
    r.write(
        "apps/cloudflare-execution/wrangler.example.jsonc",
        &serde_json::to_string_pretty(&json!({
            "name": "contextful-execution",
            "main": "src/index.ts",
            "workers_dev": false,
            "containers": [{"name": "contextful-execution", "class_name": "D1ExecutionContainer", "image": "./Dockerfile.conformance", "image_build_context": "../../.tmp/cloudflare-conformance-image"}],
            "d1_databases": [{"binding": "EXECUTION_DB", "database_name": "replace-with-disposable-d1-name", "database_id": "00000000-0000-0000-0000-000000000000"}],
            "r2_buckets": [{"binding": "EXECUTION_BLOBS", "bucket_name": "replace-with-disposable-r2-bucket"}]
        }))
        .unwrap(),
    );
    r.write(".gitignore", "/target\nnode_modules/\n.tmp/\n.fake-d1\nwrangler.deploy.jsonc\n");
    r.commit("apps");
    r
}

fn deploy(r: &Repo, app: &str, bin: &Path, env: &[(&str, &str)]) -> Output {
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful-ci"));
    cmd.args(["deploy", "cloudflare", "--app", app]).current_dir(&r.root).env_remove("CARGO_TARGET_DIR").env("PATH", path);
    cmd.env_remove("CLOUDFLARE_API_TOKEN").env_remove("CLOUDFLARE_ACCOUNT_ID");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// A deploy without either Cloudflare variable raises `DeployCredentialsAbsent`, naming it, before any command runs.
// spec: assurance.deploy-apps.credentials-absent@72bd4dd5
#[test]
fn a_deploy_without_either_cloudflare_variable_names_it_before_any_command() {
    let r = apps_repo();
    let bin = tempfile::tempdir().unwrap();
    let log = bin.path().join("calls.log");
    fakes(bin.path(), &log);
    fake_wrangler(&r, &log, "http://127.0.0.1:9", false);
    for (env, missing) in [
        (vec![], "CLOUDFLARE_API_TOKEN"),
        (vec![("CLOUDFLARE_API_TOKEN", "t")], "CLOUDFLARE_ACCOUNT_ID"),
        (vec![("CLOUDFLARE_API_TOKEN", "t"), ("CLOUDFLARE_ACCOUNT_ID", "")], "CLOUDFLARE_ACCOUNT_ID"),
        (vec![("CLOUDFLARE_API_TOKEN", ""), ("CLOUDFLARE_ACCOUNT_ID", "a")], "CLOUDFLARE_API_TOKEN"),
    ] {
        let o = deploy(&r, "gateway", bin.path(), &env);
        assert!(!o.status.success());
        let err = stderr(&o);
        assert!(err.contains("DeployCredentialsAbsent") && err.contains(missing), "{env:?}: {err}");
    }
    assert!(!log.exists(), "a command ran without credentials: {}", std::fs::read_to_string(&log).unwrap_or_default());
}

/// The execution app's D1 database and R2 bucket are created when absent and their identifiers reach only the untracked rendered config; the release is packaged in the Dockerfile's builder before `wrangler deploy`; the output carries no identifier or URL.
// spec: assurance.deploy-apps.resources@6bc0be40
// spec: assurance.deploy-apps.container-image@32055926
// spec: assurance.deploy-apps.quiet-output@b423409e
// spec: assurance.deploy-apps.expected-status@df2400d5
#[test]
fn the_execution_deploy_creates_absent_resources_packages_the_release_and_prints_no_identifier() {
    let r = apps_repo();
    let bin = tempfile::tempdir().unwrap();
    let log = bin.path().join("calls.log");
    fakes(bin.path(), &log);
    let url = answering(404, 4);
    fake_wrangler(&r, &log, &url, false);
    let creds = [("CLOUDFLARE_API_TOKEN", "t"), ("CLOUDFLARE_ACCOUNT_ID", "a")];
    let o = deploy(&r, "cloudflare-execution", bin.path(), &creds);
    let out = format!("{}{}", String::from_utf8_lossy(&o.stdout), stderr(&o));
    assert!(o.status.success(), "{out}");
    assert!(out.contains("404"), "the health status is not printed: {out}");
    assert!(!out.contains(D1_UUID) && !out.contains("127.0.0.1") && !out.contains("bbbbbbbb"), "an identifier or URL is printed: {out}");

    let calls = std::fs::read_to_string(&log).unwrap();
    let calls: Vec<&str> = calls.lines().collect();
    let at = |prefix: &str| calls.iter().position(|c| c.starts_with(prefix)).unwrap_or_else(|| panic!("no `{prefix}` in {calls:#?}"));
    let order = [
        "npm ci",
        "wrangler d1 list --json",
        "wrangler d1 create contextful-execution-db",
        "wrangler r2 bucket info contextful-execution-blobs",
        "wrangler r2 bucket create contextful-execution-blobs",
        "docker run",
        "npm run prepare-image",
        "wrangler deploy --config wrangler.deploy.jsonc",
    ];
    let positions: Vec<usize> = order.iter().map(|p| at(p)).collect();
    assert!(positions.is_sorted(), "{calls:#?}");
    let docker = calls[at("docker run")];
    assert!(docker.contains("rust:1.97-alpine@sha256:3c38f3f82c2f3d73da3b38e18d279393a04cb43ddded0e35088a8c3324d40900"), "{docker}");
    assert!(docker.contains("release --profile contextful-full --target x86_64-unknown-linux-musl"), "{docker}");
    let prepare = calls[at("npm run prepare-image")];
    assert!(prepare.contains("contextful-full-0.17.1-x86_64-unknown-linux-musl.tar.gz") && prepare.contains("cloudflare-d1.compatibility.json"), "{prepare}");

    let rendered: Value = serde_json::from_str(&std::fs::read_to_string(r.root.join("apps/cloudflare-execution/wrangler.deploy.jsonc")).unwrap()).unwrap();
    assert_eq!(rendered["d1_databases"][0]["database_id"], D1_UUID);
    assert_eq!(rendered["d1_databases"][0]["database_name"], "contextful-execution-db");
    assert_eq!(rendered["r2_buckets"][0]["bucket_name"], "contextful-execution-blobs");
    assert_eq!(rendered["workers_dev"], true);
    assert_eq!(r.git(&["status", "--porcelain"]), "", "the rendered config is tracked");
    assert!(!std::fs::read_to_string(r.root.join("apps/cloudflare-execution/wrangler.example.jsonc")).unwrap().contains(D1_UUID));

    // A second deploy finds the database and creates none.
    std::fs::remove_file(&log).unwrap();
    let url = answering(404, 4);
    fake_wrangler(&r, &log, &url, false);
    let o = deploy(&r, "cloudflare-execution", bin.path(), &creds);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(!std::fs::read_to_string(&log).unwrap().contains("d1 create"));
}

/// A status outside the app's expected set raises `DeployUnhealthy`; a failing wrangler prints with its URL and identifier replaced.
// spec: assurance.deploy-apps.health@1d82ce42
#[test]
fn an_unexpected_status_is_unhealthy_and_a_failed_deploy_prints_redacted() {
    let r = apps_repo();
    let bin = tempfile::tempdir().unwrap();
    let log = bin.path().join("calls.log");
    fakes(bin.path(), &log);
    let creds = [("CLOUDFLARE_API_TOKEN", "t"), ("CLOUDFLARE_ACCOUNT_ID", "a")];

    let url = answering(503, 4);
    fake_wrangler(&r, &log, &url, false);
    let o = deploy(&r, "gateway", bin.path(), &creds);
    assert!(o.status.success(), "the gateway's unconfigured 503 is unhealthy: {}", stderr(&o));

    let url = answering(500, 4);
    fake_wrangler(&r, &log, &url, false);
    let o = deploy(&r, "gateway", bin.path(), &creds);
    let err = stderr(&o);
    assert!(!o.status.success() && err.contains("DeployUnhealthy") && err.contains("500"), "{err}");
    assert!(!err.contains("127.0.0.1"), "{err}");

    fake_wrangler(&r, &log, "https://contextful-gateway.example.workers.dev", true);
    let o = deploy(&r, "gateway", bin.path(), &creds);
    let err = stderr(&o);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("deploy failed") && !err.contains("workers.dev") && !err.contains("0123456789abcdef0123456789abcdef"), "{err}");
}
