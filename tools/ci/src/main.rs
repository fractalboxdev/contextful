//! `contextful-ci` — the gate's stages as typed subcommands. A contributor and the
//! pull-request workflow invoke the identical command.

mod deny;
mod allowlist;
mod measure;
mod release;
mod probe;
mod stage;
mod tag;
mod footprint;
mod topology;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use contextful_eval::ledger::Tier;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use stage::STAGES;
const ACCEPTANCE_PACKAGE: &str = "contextful-acceptance";
const ACCEPTANCE_DIR: &str = "crates/acceptance";
/// The features stage's own target directory, under the workspace root.
const FEATURES_TARGET: &str = "target/features";
const REFACTOR_TRAILER: &str = "refactor";
/// Free disk a stage needs under the workspace before it begins work: 2 GiB
/// (`assurance.gate.free-disk`).
const STAGE_FREE_DISK_KIB: u64 = 2 * 1024 * 1024;
/// The exit code of a stage refused for disk (`assurance.gate.free-disk`), `ENOSPC`'s number.
const DISK_EXIT: i32 = 28;
/// Wall clock one test-first execution against the base runs for, its build excluded: 300 s
/// (`assurance.test.base-run-bound`). A run still going is killed with its process group
/// and counts red, because a test that does not finish at base does not pass there.
const BASE_RUN_BOUND_SECS: u64 = 300;
/// Cargo output naming a fault of the machine or the base workspace rather than of the
/// change's tests (`assurance.test.base-unrunnable`).
const INFRASTRUCTURE_FAULTS: [&str; 7] = [
    "No space left on device",
    "failed to load manifest",
    "failed to parse manifest",
    "could not find `Cargo.toml`",
    "failed to get `",
    "failed to download",
    "failed to select a version",
];
/// The toolchain the Lean models pin; its presence makes Lean a test dependency.
const LEAN_PIN: &str = "formal/lean-toolchain";
/// Set for every test process once Lean is provisioned, so a Lean-backed test fails
/// instead of skipping.
const REQUIRE_LEAN: &str = "CONTEXTFUL_REQUIRE_LEAN";

/// The target the decision module's WebAssembly build compiles for
/// (`assurance.structure-tree.decision-module`).
const WASM_TARGET: &str = "wasm32-unknown-unknown";
const REQUIRE_WASM: &str = "CONTEXTFUL_REQUIRE_WASM";
const ELAN_INIT: &str = "https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh";

#[derive(Parser)]
#[command(name = "contextful-ci", about = "The gate's stages")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run every stage in order, or the named stages.
    Gate {
        /// A stage, or a part `<stage>.<part>` of a split stage, as `contextful-ci stages
        /// --parts` prints it; repeatable. Defaults to every stage. A subset runs in run order.
        #[arg(long = "stage")]
        stages: Vec<String>,
        /// Also run every stage whose output a selected stage reads.
        #[arg(long)]
        predecessors: bool,
        /// The revision the change is measured against.
        #[arg(long, default_value = "origin/HEAD")]
        base: String,
        /// Overrides the wall clock of one test-first execution against the base.
        #[arg(long, hide = true, default_value_t = BASE_RUN_BOUND_SECS)]
        base_bound_secs: u64,
    },
    /// Print the stage names, one per line, in run order.
    Stages {
        /// Print what the pull-request workflow dispatches instead: each stage, a split stage
        /// as its parts.
        #[arg(long)]
        parts: bool,
    },
    /// Hold every key in a git-tracked `.env*` file to ciphertext under a scope comment.
    Secrets,
    /// Resolve every `mirrors:` comment under crates/, tools/ and apps/ to a clause id.
    Mirrors,
    /// Hold the workspace's dependency graph to the topology contract's rules.
    Topology,
    /// Compress one profile's release artifact and hold it to the profile's budget, and its
    /// dynamic dependencies to the platform C library. Reads the budget from the fragment
    /// under the working directory. With `--build`, build each named profile, every profile
    /// when none is named, as the static-linked Linux target first.
    Footprint {
        /// The profile the artifact was built as, e.g. `contextful-full`; repeatable with `--build`.
        #[arg(long, required_unless_present = "build")]
        profile: Vec<String>,
        /// The ELF artifact.
        #[arg(required_unless_present = "build", conflicts_with = "build")]
        artifact: Option<PathBuf>,
        /// Build each profile's static-linked Linux artifact, then hold it to its budget.
        #[arg(long)]
        build: bool,
        /// With `--build`, print each build command and build nothing.
        #[arg(long, requires = "build")]
        plan: bool,
    },
    /// Hold `deny.toml` to the profile-wide dependency refusals and run cargo-deny's bans
    /// over each profile's graph.
    Deny,
    /// Hold the connector authoring dependency allowlist to its required and banned
    /// entries, and the connector packages' direct dependencies to it.
    Allowlist,
    /// Build each profile for a release target and package its archive, checksum and SBOM.
    Release {
        /// A profile to build; repeatable. Defaults to every profile shipping for the target.
        #[arg(long = "profile")]
        profiles: Vec<String>,
        /// A release target; repeatable. Defaults to the one this host builds natively.
        #[arg(long = "target")]
        targets: Vec<String>,
        /// The directory the builds compile into.
        #[arg(long, default_value = release::TARGET_DIR)]
        target_dir: PathBuf,
        /// The directory receiving archives, checksums and SBOMs.
        #[arg(long, default_value = "dist")]
        out: PathBuf,
        /// Print every (profile, target) cell of the release matrix and build nothing.
        #[arg(long)]
        plan: bool,
    },
    /// Build the edge profile for `wasm32-wasip2` and record its compressed size under the
    /// scheduled ledger entry; a failed build records nothing and exits 0.
    WasiProbe {
        /// The directory the build compiles into, removed afterwards.
        #[arg(long, default_value = release::WASI_TARGET_DIR)]
        target_dir: PathBuf,
    },
    /// Write the package-manager formulae and `SHA256SUMS` over a directory of release archives.
    Formula {
        /// The directory `contextful-ci release` packaged every target into.
        #[arg(long, default_value = "dist")]
        dist: PathBuf,
        /// The URL the archives download from.
        #[arg(long)]
        base_url: String,
    },
    /// Resolve the target ledger and run its entries, or render their status.
    Measure {
        /// The tier to run; repeatable. Defaults to the gate tier.
        #[arg(long = "tier", value_parser = ["gate", "trend", "scheduled", "all"])]
        tiers: Vec<String>,
        /// Write `evals/ledger.md` from the ledger instead of running any entry.
        #[arg(long)]
        status: bool,
        /// With `--status`, refuse when the committed `evals/ledger.md` differs.
        #[arg(long, requires = "status")]
        check: bool,
    },
    /// Deploy-time checks.
    Deploy {
        #[command(subcommand)]
        cmd: DeployCmd,
    },
    /// Cut the signed annotated release tag `v0.<closed>.<patch>` on HEAD once the tree is
    /// clean, the default branch reaches it, the version rises and matches the workspace's,
    /// and every gate stage passes.
    Tag {
        /// The default branch HEAD must be reachable from.
        #[arg(long, default_value = "origin/HEAD")]
        branch: String,
        /// The revision the gate's test-first stage measures HEAD against.
        #[arg(long, default_value = "HEAD~1")]
        base: String,
    },
}

#[derive(Subcommand)]
enum DeployCmd {
    /// Hold the probe table to the hostname descriptors, then send each published hostname
    /// one anonymous `GET /` and judge its answer against the descriptor's gate.
    Probe {
        /// The directory holding `hostnames/*.toml` and `probe.toml`, relative to the
        /// repository root.
        #[arg(long, default_value = "deploy")]
        dir: PathBuf,
        /// `<hostname>=<base url>`: send that hostname's probe to the base URL; repeatable.
        #[arg(long = "resolve")]
        resolve: Vec<String>,
    },
}

/// A refusal the gate reports by its registered error name.
#[derive(Debug)]
struct Refusal {
    code: &'static str,
    message: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Refusal {}

fn refuse(code: &'static str, message: String) -> anyhow::Error {
    Refusal { code, message }.into()
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Stages { parts: false } => {
            STAGES.iter().for_each(|s| println!("{s}"));
            Ok(())
        }
        Cmd::Stages { parts: true } => repo_root().and_then(|root| dispatched(&root)).map(|all| all.iter().for_each(|s| println!("{s}"))),
        Cmd::Gate { stages, predecessors, base, base_bound_secs } => gate(&stages, predecessors, &base, Duration::from_secs(base_bound_secs)),
        Cmd::Secrets => repo_root().and_then(|root| secrets(&root)),
        Cmd::Mirrors => repo_root().and_then(|root| mirrors(&root)),
        Cmd::Topology => repo_root().and_then(|root| topology::check(&root)),
        Cmd::Footprint { profile, artifact, build, plan } => std::env::current_dir().map_err(Into::into).and_then(|root| match artifact {
            Some(artifact) if profile.len() == 1 => footprint::check(&root, &profile[0], &artifact),
            Some(_) => bail!("an artifact is measured as exactly one `--profile`"),
            None if build => {
                let profiles = if profile.is_empty() { topology::PROFILES.iter().map(|p| p.to_string()).collect() } else { profile };
                footprint::build(&root, &profiles, plan)
            }
            None => bail!("pass an artifact, or `--build`"),
        }),
        Cmd::Deny => repo_root().and_then(|root| deny::check(&root)),
        Cmd::Allowlist => repo_root().and_then(|root| allowlist::check(&root)),
        Cmd::Release { profiles, targets, target_dir, out, plan } => repo_root().and_then(|root| {
            if plan {
                return release::plan(&profiles, &targets);
            }
            release::release(&root, &profiles, &targets, &root.join(target_dir), &out)
        }),
        Cmd::WasiProbe { target_dir } => repo_root().and_then(|root| release::wasi_probe(&root, &root.join(target_dir))),
        Cmd::Formula { dist, base_url } => repo_root().and_then(|root| {
            release::formulae(&root, &dist, &base_url).map(|written| written.iter().for_each(|p| println!("formula: {}", p.display())))
        }),
        Cmd::Tag { branch, base } => tag::tag(&branch, &base),
        Cmd::Deploy { cmd: DeployCmd::Probe { dir, resolve } } => repo_root().and_then(|root| probe::run(&root.join(dir), &resolve)),
        Cmd::Measure { tiers, status, check } => repo_root().and_then(|root| {
            if status {
                return measure::status(&root, check);
            }
            let all = [Tier::Gate, Tier::Trend, Tier::Scheduled];
            let mut selected: Vec<Tier> = Vec::new();
            for t in &tiers {
                match t.as_str() {
                    "gate" => selected.push(Tier::Gate),
                    "trend" => selected.push(Tier::Trend),
                    "scheduled" => selected.push(Tier::Scheduled),
                    _ => selected.extend(all),
                }
            }
            if selected.is_empty() {
                selected.push(Tier::Gate);
            }
            selected.sort();
            selected.dedup();
            measure::run(&root, &selected)
        }),
    };
    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(exit_code(&e));
    }
}

/// A child process that failed: the command, and the code or signal it ended with.
#[derive(Debug)]
struct Exited {
    what: String,
    code: Option<i32>,
    signal: Option<i32>,
}

impl std::fmt::Display for Exited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.code, self.signal) {
            (_, Some(s)) => write!(f, "`{}` was killed by signal {s}", self.what),
            (Some(c), None) => write!(f, "`{}` exited {c}", self.what),
            (None, None) => write!(f, "`{}` ended without an exit code", self.what),
        }
    }
}

impl std::error::Error for Exited {}

fn exited(what: String, status: ExitStatus) -> anyhow::Error {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
    #[cfg(not(unix))]
    let signal = None;
    Exited { what, code: status.code(), signal }.into()
}

/// [`exited`] for a captured child, its trimmed stderr leading the diagnostics.
fn exited_output(what: &str, out: &std::process::Output) -> anyhow::Error {
    exited(what.to_string(), out.status).context(format!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim()))
}

/// The exit code a failed run propagates: a child's own code, `128 + n` for a child killed
/// by signal `n`, 28 for the disk precondition, and 1 otherwise.
fn exit_code(e: &anyhow::Error) -> i32 {
    if e.downcast_ref::<Refusal>().is_some_and(|r| r.code == "BuildDiskPrecondition") {
        return DISK_EXIT;
    }
    match e.downcast_ref::<Exited>() {
        Some(Exited { signal: Some(s), .. }) => 128 + s,
        Some(Exited { code: Some(c), .. }) if *c != 0 => *c,
        _ => 1,
    }
}

fn repo_root() -> Result<PathBuf> {
    Ok(PathBuf::from(git(&["rev-parse", "--show-toplevel"])?))
}

/// The stages whose work splits into parts the pull-request workflow dispatches one check
/// each, so each part fits one stage's wall clock (`assurance.build.profile-build`,
/// `assurance.gate.budget-stage`).
const SPLIT: [&str; 2] = ["features", "budget"];

/// The parts of a split `stage`: the features stage's `packages`, every featured package but
/// the binary, then `binary-<run>` per run of the binary package; the budget stage's
/// `<profile>` per profile the binary declares, its name after `contextful-`.
fn parts(root: &Path, stage: &str) -> Result<Vec<String>> {
    Ok(match stage {
        "features" => {
            let featured = featured_packages(root)?;
            let binary = featured.iter().filter(|p| p.name == topology::BINARY).flat_map(Featured::runs);
            std::iter::once("packages".to_string()).chain(binary.map(|(label, _)| format!("binary-{label}"))).collect()
        }
        "budget" => topology::declared_profiles(root)?.iter().map(|p| p.strip_prefix("contextful-").unwrap_or(p).to_string()).collect(),
        _ => Vec::new(),
    })
}

/// What the pull-request workflow dispatches, in run order: each stage, a split stage as
/// `<stage>.<part>` per part.
fn dispatched(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for stage in STAGES {
        if SPLIT.contains(&stage) {
            out.extend(parts(root, stage)?.into_iter().map(|p| format!("{stage}.{p}")));
        } else {
            out.push(stage.to_string());
        }
    }
    Ok(out)
}

fn gate(named: &[String], predecessors: bool, base: &str, bound: Duration) -> Result<()> {
    let root = repo_root()?;
    // A named part selects its stage and narrows it to the named parts; a stage named whole
    // runs whole.
    let mut stages: Vec<String> = Vec::new();
    let mut whole: Vec<&str> = Vec::new();
    let mut narrowed: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
    for name in named {
        let (stage, part) = name.split_once('.').map_or((name.as_str(), None), |(s, p)| (s, Some(p)));
        let Some(stage) = STAGES.iter().copied().find(|s| *s == stage) else {
            bail!("no stage `{stage}`; the stages are {}", STAGES.join(", "));
        };
        stages.push(stage.to_string());
        match part {
            None => whole.push(stage),
            Some(part) if SPLIT.contains(&stage) && parts(&root, stage)?.iter().any(|p| p == part) => {
                narrowed.entry(stage).or_default().push(part.to_string())
            }
            Some(_) => bail!("no part `{name}`; the parts are {}", dispatched(&root)?.join(", ")),
        }
    }
    narrowed.retain(|stage, _| !whole.contains(stage));
    for stage in stage::select(&root, &stages, predecessors)? {
        eprintln!("--- stage {stage}");
        free_disk(&root, stage)?;
        let mut mark = stage::mark();
        let outcome = run_stage(&root, stage, narrowed.get(stage).map(Vec::as_slice), base, bound);
        stage::report(stage, &mut mark, &outcome);
        outcome?;
        // A passing stage leaves no build behind (`assurance.build.target-dir-per-stage`); a
        // failing one keeps its directory for diagnosis.
        let _ = std::fs::remove_dir_all(stage_target(&root, stage));
    }
    Ok(())
}

/// One stage's work, apart from the disk precondition and its report; `only` the named parts
/// of a split stage.
fn run_stage(root: &Path, stage: &str, only: Option<&[String]>, base: &str, bound: Duration) -> Result<()> {
    match stage {
        "pins" => stage::pins(root)?,
        "toolchain" => stage::toolchain(root)?,
        "schema" => {
            secrets(root)?;
            mirrors(root)?;
            stage::regenerate(root)?;
            run_staged(root, stage, &["run", "--locked", "-q", "-p", "contextful-spec", "--", "lint"])?
        }
        "test-first" => {
            provision_lean(root)?;
            provision_wasm(root)?;
            test_first(root, base, bound)?
        }
        "workspace" => {
            provision_lean(root)?;
            provision_wasm(root)?;
            workspace(root)?
        }
        "acceptance" => acceptance(root)?,
        "evaluate" => measure::evaluate(root)?,
        "features" => features(root, only)?,
        "crate-graph" => {
            committed_lock(root)?;
            topology::check(root)?;
            deny::check(root)?;
            allowlist::check(root)?
        }
        "budget" => {
            // The footprint builds run here, apart from the evaluate stage
            // (`assurance.gate.budget-stage`).
            let declared = topology::declared_profiles(root)?.into_iter();
            let short = |p: &str| p.strip_prefix("contextful-").unwrap_or(p).to_string();
            let profiles: Vec<String> = declared.filter(|p| only.is_none_or(|o| o.contains(&short(p)))).map(str::to_string).collect();
            if profiles.is_empty() {
                println!("budget: no package declares a profile");
            } else {
                footprint::build(root, &profiles, false)?;
                let _ = std::fs::remove_dir_all(root.join(footprint::TARGET_DIR));
            }
        }
        "connectors" => stage::connectors(root)?,
        "surfaces" => stage::typescript(root)?,
        "formal" => stage::formal(root)?,
        _ => unreachable!(),
    }
    Ok(())
}

/// The target directory `stage` builds into, under the workspace root and apart from every
/// other stage's (`assurance.build.target-dir-per-stage`): stages under different feature
/// unification share no artifacts, so peak disk is one stage's.
fn stage_target(root: &Path, stage: &str) -> PathBuf {
    root.join("target").join(stage)
}

/// Run `cargo args` in `stage`'s own target directory.
fn run_staged(root: &Path, stage: &str, args: &[&str]) -> Result<()> {
    let status = Command::new("cargo").args(args).env("CARGO_TARGET_DIR", stage_target(root, stage)).current_dir(root).status()?;
    if !status.success() {
        bail!("`cargo {}` exited {}", args.join(" "), status.code().unwrap_or(-1));
    }
    Ok(())
}

/// Refuse a `Cargo.lock` that differs from the one the measured commit records: `cargo
/// run` without `--locked` rewrites a stale lock before this binary starts, and the
/// crate-graph rules then resolve a graph the commit does not (`assurance.gate.locked-resolve`).
fn committed_lock(root: &Path) -> Result<()> {
    let out = Command::new("git")
        .args(["status", "--porcelain", "--", "Cargo.lock"])
        .current_dir(root)
        .output()
        .context("running git")?;
    if !out.status.success() {
        return Err(exited_output("git status", &out));
    }
    let changed = String::from_utf8_lossy(&out.stdout);
    if !changed.trim().is_empty() {
        bail!(
            "`Cargo.lock` differs from the committed one ({}); resolve with `cargo run --locked` and commit the lock the manifests need",
            changed.trim()
        );
    }
    Ok(())
}

/// Refuse a stage starting with less than [`STAGE_FREE_DISK_KIB`] free on the filesystem
/// holding the workspace, read through POSIX `df -Pk`.
fn free_disk(root: &Path, stage: &str) -> Result<()> {
    let out = Command::new("df").arg("-Pk").arg(root).output().context("running df")?;
    if !out.status.success() {
        return Err(exited_output(&format!("df -Pk {}", root.display()), &out));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // The fourth field of the data line is the available space in KiB.
    let available: u64 = text
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().nth(3))
        .and_then(|f| f.parse().ok())
        .with_context(|| format!("df -Pk printed no available figure: {text}"))?;
    if available < STAGE_FREE_DISK_KIB {
        return Err(refuse(
            "BuildDiskPrecondition",
            format!("stage `{stage}` starts with {} MiB free under {}, below the 2048 MiB floor", available / 1024, root.display()),
        ));
    }
    Ok(())
}

/// Every workspace package's suite in one cargo invocation over the union of their
/// features, so the bundled SQL engine compiles once in the stage
/// (`assurance.build.one-engine-build`).
fn workspace(root: &Path) -> Result<()> {
    let mut args = vec!["test", "--workspace"];
    if root.join(ACCEPTANCE_DIR).join("Cargo.toml").exists() {
        args.extend(["--exclude", ACCEPTANCE_PACKAGE]);
    }
    run_staged(root, "workspace", &args)
}

/// The feature combinations the workspace stage's unified build does not reach
/// (`assurance.build.staged-feature-runs`): each workspace package declaring a feature other
/// than `default` runs its suite alone, once with no features, once with every feature, and
/// once per feature set its manifest lists under `[package.metadata.contextful]
/// feature-runs` (`assurance.build.profile-build`). `only` narrows the stage to its named
/// parts: `packages`, every package but the binary, and `binary-<run>`, one run of the binary.
/// `cargo test -p` resolves the selected package's features without its dependents', so the
/// store adapter's write suites run with the read face off
/// (`topology.package.store-write-engine-free`) and the policy package's without `exchange`.
/// The stage builds into `target/features`, reclaimed once it passes, because a
/// different feature unification shares no artifacts with the workspace build
/// (`assurance.build.target-dir-per-stage`).
fn features(root: &Path, only: Option<&[String]>) -> Result<()> {
    let featured = featured_packages(root)?;
    if featured.is_empty() {
        eprintln!("features: no workspace package declares a feature");
    }
    let target = root.join(FEATURES_TARGET);
    for package in &featured {
        let name = &package.name;
        let binary = name == topology::BINARY;
        let selected = |label: &str| {
            only.is_none_or(|o| o.iter().any(|p| if binary { p.strip_prefix("binary-") == Some(label) } else { p == "packages" }))
        };
        for (_, combination) in package.runs().into_iter().filter(|(label, _)| selected(label)) {
            let shown = combination.join(" ");
            eprintln!("features: {name} {shown}");
            let status = Command::new("cargo")
                .args(["test", "-p", name])
                .args(&combination)
                .env("CARGO_TARGET_DIR", &target)
                .current_dir(root)
                .status()?;
            if !status.success() {
                return Err(exited(format!("cargo test -p {name} {shown}"), status));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&target);
    Ok(())
}

// ---------------------------------------------------------------- lean

/// Install elan when absent and the toolchain `formal/lean-toolchain` pins, put elan's
/// `bin` first on `PATH`, pin `ELAN_TOOLCHAIN` to it, and set `CONTEXTFUL_REQUIRE_LEAN=1` for every test process
/// this gate run starts. A tree pinning no toolchain is left untouched.
fn provision_lean(root: &Path) -> Result<()> {
    let Ok(pin) = std::fs::read_to_string(root.join(LEAN_PIN)) else { return Ok(()) };
    provision_lean_pin(root, pin.trim())
}

/// Provision the Lean toolchain `pin` names, as [`provision_lean`] describes.
fn provision_lean_pin(root: &Path, pin: &str) -> Result<()> {
    let elan_home = std::env::var_os("ELAN_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".elan")))
        .context("neither ELAN_HOME nor HOME is set")?;
    let bin = elan_home.join("bin");
    if !bin.join("elan").exists() {
        eprintln!("lean: installing elan into {}", elan_home.display());
        let script = format!("curl -sSfL {ELAN_INIT} | sh -s -- -y --no-modify-path --default-toolchain none");
        run(root, "sh", &["-c", &script])?;
    }
    let elan = bin.join("elan");
    let listed = Command::new(&elan).args(["toolchain", "list"]).output().context("running elan")?;
    if !String::from_utf8_lossy(&listed.stdout).lines().any(|l| l.split_whitespace().next() == Some(pin)) {
        run(root, &elan.to_string_lossy(), &["toolchain", "install", pin])?;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let joined = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path)))?;
    std::env::set_var("PATH", joined);
    // A test runs `lean` from its package directory, where no `lean-toolchain` sits
    // above it and a fresh elan has no default to fall back to.
    std::env::set_var("ELAN_TOOLCHAIN", pin);
    std::env::set_var(REQUIRE_LEAN, "1");
    eprintln!("lean: {pin} provisioned; {REQUIRE_LEAN}=1");
    Ok(())
}

/// Install the `wasm32-unknown-unknown` standard library through rustup when the toolchain
/// lacks it, and set `CONTEXTFUL_REQUIRE_WASM=1` for every test process this gate run
/// starts, so a WebAssembly-backed test fails rather than skips.
fn provision_wasm(root: &Path) -> Result<()> {
    if !wasm_target_installed(root) {
        eprintln!("wasm: installing {WASM_TARGET}");
        run(root, "rustup", &["target", "add", WASM_TARGET])?;
        if !wasm_target_installed(root) {
            bail!("`rustup target add {WASM_TARGET}` left the toolchain without the target");
        }
    }
    std::env::set_var(REQUIRE_WASM, "1");
    eprintln!("wasm: {WASM_TARGET} provisioned; {REQUIRE_WASM}=1");
    Ok(())
}

/// Whether the toolchain `rustc` resolves in `root` holds the target's standard library.
fn wasm_target_installed(root: &Path) -> bool {
    let out = Command::new("rustc").args(["--print", "target-libdir", "--target", WASM_TARGET]).current_dir(root).output();
    out.ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .is_some_and(|dir| std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some()))
}

// ---------------------------------------------------------------- secrets

const CIPHERTEXT_PREFIX: &str = "encrypted:";
const PUBLIC_KEY_PREFIX: &str = "DOTENV_PUBLIC_KEY";
const KEY_FILE: &str = ".env.keys";
const EXAMPLE_FILE: &str = ".env.example";

/// `assurance.gate.secret-ciphertext` and `assurance.gate.secret-scope`. A finding names
/// the file and the key; no value reaches the output.
fn secrets(root: &Path) -> Result<()> {
    let files: Vec<String> = tracked(root)?.into_iter().filter(|p| file_name(p).starts_with(".env")).collect();
    let (mut plaintext, mut unscoped, mut keys) = (Vec::new(), Vec::new(), 0usize);
    for file in &files {
        if file_name(file) == KEY_FILE {
            plaintext.push(format!("{file} is tracked; the dotenvx private keys stay out of git"));
        }
        let text = std::fs::read_to_string(root.join(file)).with_context(|| format!("reading {file}"))?;
        for entry in env_entries(&text).into_iter().filter(|e| !e.key.starts_with(PUBLIC_KEY_PREFIX)) {
            keys += 1;
            if !entry.ciphertext && file_name(file) != EXAMPLE_FILE {
                plaintext.push(format!("{file}: `{}` holds a value that is not dotenvx ciphertext", entry.key));
            }
            if !entry.scoped {
                unscoped.push(format!("{file}: `{}` has no comment directly above it stating what it grants", entry.key));
            }
        }
    }
    plaintext.iter().for_each(|m| eprintln!("SecretPlaintext: {m}"));
    unscoped.iter().for_each(|m| eprintln!("SecretScopeMissing: {m}"));
    if !plaintext.is_empty() {
        return Err(refuse("SecretPlaintext", format!("{} finding(s) across tracked .env files", plaintext.len())));
    }
    if !unscoped.is_empty() {
        return Err(refuse("SecretScopeMissing", format!("{} key(s) with no scope comment", unscoped.len())));
    }
    println!("secrets: {keys} key(s) in {} file(s), each ciphertext under a scope comment", files.len());
    Ok(())
}

struct EnvEntry {
    key: String,
    ciphertext: bool,
    scoped: bool,
}

/// The assignments of a dotenv file. A quoted value may span lines; a key is scoped when
/// a comment carrying text opens its block — the run of assignment lines above it, up to
/// the first blank line.
fn env_entries(text: &str) -> Vec<EnvEntry> {
    let lines: Vec<&str> = text.lines().collect();
    let mut entries = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        let assignment = line.strip_prefix("export ").unwrap_or(line);
        let parsed = if line.is_empty() || line.starts_with('#') { None } else { assignment.split_once('=') };
        if let Some((key, value)) = parsed {
            let value = value.trim();
            let scoped = block_comment(&lines[..i]);
            let ciphertext = value.trim_start_matches(['"', '\'']).starts_with(CIPHERTEXT_PREFIX);
            entries.push(EnvEntry { key: key.trim().to_string(), ciphertext, scoped });
            if let Some(q) = value.chars().next().filter(|c| *c == '"' || *c == '\'') {
                if !value[1..].contains(q) {
                    i += 1;
                    while i < lines.len() && !lines[i].contains(q) {
                        i += 1;
                    }
                }
            }
        }
        i += 1;
    }
    entries
}

/// Whether the block above these lines opens with a comment carrying text: walk back over
/// the assignments and comments of the block, stopping at a blank line or the file's head.
fn block_comment(above: &[&str]) -> bool {
    for line in above.iter().rev().map(|l| l.trim()) {
        if line.is_empty() {
            return false;
        }
        if let Some(text) = line.strip_prefix('#') {
            if !text.trim().is_empty() {
                return true;
            }
        }
    }
    false
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

// ---------------------------------------------------------------- mirrors

const LOCK_FILE: &str = "spec/spec.lock.json";
const MIRROR_TAG: &str = "mirrors:";
const MIRROR_ROOTS: [&str; 3] = ["crates/", "tools/", "apps/"];

/// `assurance.structure-tree.mirror-unresolved`: a comment line whose text opens with the
/// tag names a clause id the lock file carries.
fn mirrors(root: &Path) -> Result<()> {
    let mut sites = Vec::new();
    for file in tracked(root)?.into_iter().filter(|p| MIRROR_ROOTS.iter().any(|r| p.starts_with(r))) {
        let Ok(text) = std::fs::read_to_string(root.join(&file)) else { continue };
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            let body = code.trim_start_matches(['/', '#', '-', '*']).trim_start();
            if body.len() < code.len() && body.starts_with(MIRROR_TAG) {
                let id = body[MIRROR_TAG.len()..].split_whitespace().next().unwrap_or("").to_string();
                sites.push((format!("{file}:{}", n + 1), id));
            }
        }
    }
    let clauses: Vec<String> = match std::fs::read_to_string(root.join(LOCK_FILE)) {
        Ok(text) => {
            let lock: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {LOCK_FILE}"))?;
            lock["clauses"]
                .as_array()
                .map(|cs| cs.iter().filter_map(|c| c["id"].as_str()).map(str::to_string).collect())
                .unwrap_or_default()
        }
        Err(_) => Vec::new(),
    };
    let unresolved: Vec<&(String, String)> = sites.iter().filter(|(_, id)| !clauses.contains(id)).collect();
    unresolved
        .iter()
        .for_each(|(site, id)| eprintln!("MirrorUnresolved: {site} names `{id}`, which is no clause of {LOCK_FILE}"));
    if !unresolved.is_empty() {
        return Err(refuse("MirrorUnresolved", format!("{} annotation(s) name no clause", unresolved.len())));
    }
    println!("mirrors: {} annotation(s) resolve to clauses", sites.len());
    Ok(())
}

// ---------------------------------------------------------------- acceptance

fn acceptance(root: &Path) -> Result<()> {
    let manifest = root.join(ACCEPTANCE_DIR).join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).with_context(|| format!("reading {ACCEPTANCE_DIR}/Cargo.toml"))?;
    let doc: toml::Table = text.parse().context("parsing the acceptance manifest")?;
    let members = workspace_packages(root)?;
    for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(deps) = doc.get(table).and_then(|v| v.as_table()) else { continue };
        for (name, spec) in deps {
            let by_path = spec.as_table().map(|t| t.contains_key("path") || t.contains_key("workspace")).unwrap_or(false);
            if by_path || members.contains(name) {
                return Err(refuse(
                    "AcceptanceLinksEngine",
                    format!("{ACCEPTANCE_DIR}/Cargo.toml [{table}] names workspace package `{name}`; drive its binary instead"),
                ));
            }
        }
    }
    run_staged(root, "acceptance", &["test", "-p", ACCEPTANCE_PACKAGE])
}

/// A workspace member declaring a feature other than `default`.
struct Featured {
    name: String,
    /// Feature sets its manifest lists under `[package.metadata.contextful] feature-runs`.
    listed: Vec<String>,
}

impl Featured {
    /// Each feature combination the package's suite runs under, labelled `none`, `all` or
    /// the listed set with every character outside `[a-z0-9-]` written `-`.
    fn runs(&self) -> Vec<(String, Vec<String>)> {
        let mut out = vec![("none".to_string(), vec!["--no-default-features".to_string()]), ("all".to_string(), vec!["--all-features".to_string()])];
        for set in &self.listed {
            let label = set.chars().map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' { c } else { '-' }).collect();
            out.push((label, vec!["--no-default-features".into(), "--features".into(), set.clone()]));
        }
        out
    }
}

/// Workspace members declaring a feature other than `default`, sorted by name.
fn featured_packages(root: &Path) -> Result<Vec<Featured>> {
    let meta = metadata(root)?;
    let mut out: Vec<Featured> = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["features"].as_object().is_some_and(|f| f.keys().any(|k| k != "default")))
        .filter_map(|p| {
            let name = p["name"].as_str().filter(|n| *n != ACCEPTANCE_PACKAGE)?.to_string();
            let listed = p["metadata"]["contextful"]["feature-runs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect();
            Some(Featured { name, listed })
        })
        .collect();
    out.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// `cargo metadata` over the workspace members alone.
fn metadata(root: &Path) -> Result<serde_json::Value> {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1", "-q"])
        .current_dir(root)
        .output()?;
    if !out.status.success() {
        return Err(exited_output("cargo metadata", &out));
    }
    serde_json::from_slice(&out.stdout).context("parsing cargo metadata")
}

/// Package names of every workspace member, from `cargo metadata`.
fn workspace_packages(root: &Path) -> Result<Vec<String>> {
    let meta = metadata(root)?;
    let names = meta["packages"]
        .as_array()
        .map(|ps| ps.iter().filter_map(|p| p["name"].as_str()).map(str::to_string).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n != ACCEPTANCE_PACKAGE)
        .collect();
    Ok(names)
}

// ---------------------------------------------------------------- test-first

fn test_first(root: &Path, base: &str, bound: Duration) -> Result<()> {
    let range = format!("{base}...HEAD");
    let sources = sources_outside_refactors(base)?;
    if sources.is_empty() {
        eprintln!("test-first: no Rust source under crates/ or tools/ changed outside `Test-First: {REFACTOR_TRAILER}` commits");
        return Ok(());
    }
    let added = git(&["diff", "--name-only", "--diff-filter=ACMR", &range])?;
    let tests: Vec<&str> = added.lines().filter(|p| is_test(p)).collect();
    if tests.is_empty() {
        return Err(refuse(
            "TestNotFirst",
            format!("{} source file(s) changed and no test under a package's tests/ did: {}", sources.len(), sources.join(", ")),
        ));
    }

    let scratch = root.join("target/test-first");
    let tree = scratch.join("tree");
    let _ = git(&["worktree", "remove", "--force", &tree.to_string_lossy()]);
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)?;
    git(&["worktree", "add", "--detach", "-q", &tree.to_string_lossy(), &merge_base(base)?])?;
    let verdict = red_against_base(root, &tree, &scratch.join("target"), &tests, bound);
    let _ = git(&["worktree", "remove", "--force", &tree.to_string_lossy()]);
    let _ = std::fs::remove_dir_all(&scratch);
    let red = verdict?;
    if red.is_empty() {
        return Err(refuse(
            "TestNotFirst",
            format!("the change's tests pass against the base source, so they specify nothing it adds: {}", tests.join(", ")),
        ));
    }
    eprintln!("test-first: red against the base in {}", red.join(", "));
    Ok(())
}

/// The tests one base invocation runs: a package's test target, filtered to the modules the
/// change touches, or the whole target when only its root file changed.
#[derive(Debug, PartialEq, Eq)]
struct Selection {
    pkg: String,
    /// `None` runs every test target of the package: the file is a helper no target names.
    target: Option<String>,
    /// Test-name filters; empty runs the whole target.
    filters: Vec<String>,
}

/// Map a changed test file to its package, test target and module filter. A file directly
/// under `tests/` is its own target; under `tests/<target>/`, the target's `main.rs` stands
/// for the whole target and any other file for its top-level module, `<mod>::`.
fn target_of(tree: &Path, test: &str) -> (String, Option<String>, Option<String>) {
    let (pkg, rest) = test.split_once("/tests/").unwrap_or((test, ""));
    let parts: Vec<&str> = rest.split('/').collect();
    let first = parts[0];
    if let Some(stem) = first.strip_suffix(".rs") {
        return (pkg.to_string(), Some(stem.to_string()), None);
    }
    let tests = tree.join(pkg).join("tests");
    let is_target = tests.join(first).join("main.rs").exists() || tests.join(format!("{first}.rs")).exists();
    if !is_target {
        return (pkg.to_string(), None, None);
    }
    let module = parts.get(1).map(|m| m.trim_end_matches(".rs")).filter(|m| *m != "main");
    (pkg.to_string(), Some(first.to_string()), module.map(|m| format!("{m}::")))
}

/// Group the changed test files into one invocation per package and target. A target's
/// root file declares its modules, so it adds no filter; a target whose root file alone
/// changed runs whole.
fn selections(tree: &Path, tests: &[&str]) -> Vec<Selection> {
    let mut out: Vec<Selection> = Vec::new();
    for t in tests {
        let (pkg, target, filter) = target_of(tree, t);
        let at = match out.iter().position(|s| s.pkg == pkg && s.target == target) {
            Some(i) => i,
            None => {
                out.push(Selection { pkg, target, filters: Vec::new() });
                out.len() - 1
            }
        };
        if let Some(f) = filter.filter(|f| !out[at].filters.contains(f)) {
            out[at].filters.push(f);
        }
    }
    out
}

/// Read a pipe on a thread of its own, delivering each chunk over a channel.
fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let Some(mut p) = pipe else { return };
        let mut buf = [0u8; 8192];
        while let Ok(n) = p.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    rx
}

/// Gather a drained pipe until it closes or `deadline` passes.
fn gather(rx: &Receiver<Vec<u8>>, deadline: Instant) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => bytes.extend_from_slice(&chunk),
            Err(RecvTimeoutError::Disconnected) => return bytes,
            Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => return bytes,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

/// How one base invocation ended.
enum BaseRun {
    Passed,
    Failed,
    Killed,
}

/// What one base invocation printed.
struct Printed {
    stdout: String,
    stderr: String,
}

/// Run `cmd` in a process group of its own, for at most `bound` when one is given; a run
/// past the bound has its whole group killed.
fn run_bounded(mut cmd: Command, bound: Option<Duration>) -> Result<(BaseRun, Printed)> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().context("spawning cargo")?;
    let pgid = child.id();
    let (out, err) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let started = Instant::now();
    let run = loop {
        if let Some(status) = child.try_wait()? {
            break if status.success() { BaseRun::Passed } else { BaseRun::Failed };
        }
        if bound.is_some_and(|b| started.elapsed() >= b) {
            let _ = Command::new("kill").args(["-KILL", "--", &format!("-{pgid}")]).status();
            let _ = child.kill();
            let _ = child.wait();
            break BaseRun::Killed;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // The group is gone, so both pipes close; the deadline guards one held elsewhere.
    let deadline = Instant::now() + Duration::from_secs(10);
    let printed = Printed {
        stdout: String::from_utf8_lossy(&gather(&out, deadline)).into_owned(),
        stderr: String::from_utf8_lossy(&gather(&err, deadline)).into_owned(),
    };
    Ok((run, printed))
}

/// The infrastructure fault cargo's own standard error names, if any: a full disk, a
/// workspace or manifest that does not load, a dependency that does not fetch. Only
/// cargo's lines are read — a top-level `error:` line, the `Caused by:` chain under it,
/// and a TOML diagnostic pointing into a `Cargo.toml` — never rustc's source excerpts or
/// standard output, both of which quote the tests' own text.
fn infrastructure_fault(stderr: &str) -> Option<String> {
    let mut in_chain = false;
    for line in stderr.lines() {
        let cargo_line = if line.starts_with("error:") {
            in_chain = false;
            true
        } else if line == "Caused by:" {
            in_chain = true;
            false
        } else if in_chain && line.starts_with("  ") && !line.trim().is_empty() {
            true
        } else {
            in_chain = false;
            let t = line.trim_start();
            if t.starts_with("--> ") && line[..line.len() - t.len()].chars().all(char::is_whitespace) && t.contains("Cargo.toml:") {
                return Some(line.trim().to_string());
            }
            false
        };
        if cargo_line && INFRASTRUCTURE_FAULTS.iter().any(|f| line.contains(f)) {
            return Some(line.trim().to_string());
        }
    }
    None
}

/// Overlay the change's test files on the base tree and run only those tests there;
/// return each selection that fails to compile, fails, or outruns its bound. A build runs
/// unbounded, and only the tests' execution answers to the bound. An infrastructure fault
/// fails the stage rather than reading as red.
///
/// A package the base lacks counts red by construction and none of its files is copied,
/// so a test directory with no manifest never stops the base workspace from loading and
/// every package that exists at base is judged by its own tests alone.
fn red_against_base(root: &Path, tree: &Path, target: &Path, tests: &[&str], bound: Duration) -> Result<Vec<String>> {
    let mut present: Vec<&str> = Vec::new();
    let mut red = Vec::new();
    for t in tests {
        let pkg = t.split("/tests/").next().unwrap_or(t);
        if !tree.join(pkg).join("Cargo.toml").exists() {
            let entry = format!("{pkg} (absent at base)");
            if !red.contains(&entry) {
                red.push(entry);
            }
            continue;
        }
        let dest = tree.join(t);
        std::fs::create_dir_all(dest.parent().unwrap())?;
        std::fs::copy(root.join(t), &dest).with_context(|| format!("overlaying {t}"))?;
        present.push(t);
    }
    for sel in selections(tree, &present) {
        let label = format!(
            "{} ({}{})",
            sel.pkg,
            sel.target.as_deref().unwrap_or("every test target"),
            if sel.filters.is_empty() { String::new() } else { format!(": {}", sel.filters.join(" ")) }
        );
        let cargo = |extra: &[&str], after: &[String]| {
            let mut cmd = Command::new("cargo");
            // Every feature, so a test compiled in only behind one still runs against the base.
            cmd.args(["test", "-q", "--all-features"]).args(extra).arg("--manifest-path").arg(tree.join(&sel.pkg).join("Cargo.toml"));
            match &sel.target {
                Some(t) => cmd.args(["--test", t]),
                None => cmd.arg("--tests"),
            };
            if !after.is_empty() {
                cmd.arg("--").args(after);
            }
            cmd.env("CARGO_TARGET_DIR", target).current_dir(tree);
            cmd
        };
        let unrunnable = |printed: &Printed| -> Result<()> {
            match infrastructure_fault(&printed.stderr) {
                Some(fault) => Err(refuse(
                    "TestFirstBaseUnrunnable",
                    format!("{label} did not run at base: `{fault}`; the machine, the base workspace or a manifest stopped it, which is no verdict on the change"),
                )),
                None => Ok(()),
            }
        };
        // Build: unbounded, since a cold base compile says nothing about the tests.
        let (built, printed) = run_bounded(cargo(&["--no-run"], &[]), None)?;
        eprint!("{}{}", printed.stdout, printed.stderr);
        if !matches!(built, BaseRun::Passed) {
            unrunnable(&printed)?;
            red.push(format!("{label}, which does not compile at base"));
            continue;
        }
        // Select: the exact names under each changed top-level module.
        let mut run_args: Vec<String> = Vec::new();
        if !sel.filters.is_empty() {
            let (listed, printed) = run_bounded(cargo(&[], &["--list".into(), "--format".into(), "terse".into()]), Some(bound))?;
            if !matches!(listed, BaseRun::Passed) {
                eprint!("{}{}", printed.stdout, printed.stderr);
                unrunnable(&printed)?;
                red.push(format!("{label}, whose tests do not list at base"));
                continue;
            }
            let names: Vec<String> = printed
                .stdout
                .lines()
                .filter_map(|l| l.strip_suffix(": test"))
                .filter(|n| sel.filters.iter().any(|f| n.starts_with(f.as_str())))
                .map(str::to_string)
                .collect();
            if names.is_empty() {
                continue;
            }
            run_args.push("--exact".into());
            run_args.extend(names);
        }
        // Run: bounded, since a test that does not finish at base does not pass there.
        let (run, printed) = run_bounded(cargo(&[], &run_args), Some(bound))?;
        eprint!("{}{}", printed.stdout, printed.stderr);
        if !matches!(run, BaseRun::Passed) {
            unrunnable(&printed)?;
        }
        match run {
            BaseRun::Passed => {}
            BaseRun::Failed => red.push(label),
            BaseRun::Killed => red.push(format!("{label}, killed at the {} s bound", bound.as_secs())),
        }
    }
    Ok(red)
}

/// Source files changed by the range's commits that do not carry `Test-First: refactor`.
fn sources_outside_refactors(base: &str) -> Result<Vec<String>> {
    let mut sources: Vec<String> = Vec::new();
    for commit in git(&["rev-list", &format!("{base}..HEAD")])?.lines() {
        let trailer = git(&["log", "-1", "--format=%(trailers:key=Test-First,valueonly)", commit])?;
        if trailer.lines().any(|l| l.trim() == REFACTOR_TRAILER) {
            continue;
        }
        let files = git(&["diff-tree", "--no-commit-id", "--name-only", "-r", "--root", commit])?;
        for p in files.lines().filter(|p| is_source(p)) {
            if !sources.iter().any(|s| s == p) {
                sources.push(p.to_string());
            }
        }
    }
    Ok(sources)
}

fn merge_base(base: &str) -> Result<String> {
    git(&["merge-base", base, "HEAD"])
}

/// Every Rust file under `crates/` or `tools/` outside a `tests/` directory: `src/`,
/// `build.rs`, `benches/` and `examples/` alike.
fn is_source(p: &str) -> bool {
    (p.starts_with("crates/") || p.starts_with("tools/")) && p.ends_with(".rs") && !is_test(p)
}

fn is_test(p: &str) -> bool {
    (p.starts_with("crates/") || p.starts_with("tools/")) && p.ends_with(".rs") && p.contains("/tests/")
}

// ---------------------------------------------------------------- process

fn git(args: &[&str]) -> Result<String> {
    let out = Command::new("git").args(args).output().context("running git")?;
    if !out.status.success() {
        return Err(exited_output(&format!("git {}", args.join(" ")), &out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Paths git tracks under `root`.
fn tracked(root: &Path) -> Result<Vec<String>> {
    let out = Command::new("git").args(["ls-files", "-z"]).current_dir(root).output().context("running git")?;
    if !out.status.success() {
        return Err(exited_output("git ls-files", &out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect())
}

fn run(root: &Path, program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program).args(args).current_dir(root).status()?;
    if !status.success() {
        return Err(exited(format!("{program} {}", args.join(" ")), status));
    }
    Ok(())
}
