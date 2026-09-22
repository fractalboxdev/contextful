//! `contextful-ci` — the gate's stages as typed subcommands. A contributor and the
//! pull-request workflow invoke the identical command.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Gate stages in run order. The pull-request workflow dispatches each as its own check.
const STAGES: [&str; 4] = ["schema", "test-first", "workspace", "acceptance"];
const ACCEPTANCE_PACKAGE: &str = "contextful-acceptance";
const ACCEPTANCE_DIR: &str = "crates/acceptance";
const REFACTOR_TRAILER: &str = "refactor";
/// The toolchain the Lean models pin; its presence makes Lean a test dependency.
const LEAN_PIN: &str = "formal/lean-toolchain";
/// Set for every test process once Lean is provisioned, so a Lean-backed test fails
/// instead of skipping.
const REQUIRE_LEAN: &str = "CONTEXTFUL_REQUIRE_LEAN";
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
        /// A stage to run; repeatable. Defaults to every stage.
        #[arg(long = "stage", value_parser = clap::builder::PossibleValuesParser::new(STAGES))]
        stages: Vec<String>,
        /// The revision the change is measured against.
        #[arg(long, default_value = "origin/HEAD")]
        base: String,
    },
    /// Print the stage names, one per line, in run order.
    Stages,
    /// Hold every key in a git-tracked `.env*` file to ciphertext under a scope comment.
    Secrets,
    /// Resolve every `mirrors:` comment under crates/, tools/ and apps/ to a clause id.
    Mirrors,
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
        Cmd::Stages => {
            STAGES.iter().for_each(|s| println!("{s}"));
            Ok(())
        }
        Cmd::Gate { stages, base } => gate(&stages, &base),
        Cmd::Secrets => repo_root().and_then(|root| secrets(&root)),
        Cmd::Mirrors => repo_root().and_then(|root| mirrors(&root)),
    };
    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}

fn repo_root() -> Result<PathBuf> {
    Ok(PathBuf::from(git(&["rev-parse", "--show-toplevel"])?))
}

fn gate(selected: &[String], base: &str) -> Result<()> {
    let root = repo_root()?;
    for stage in STAGES.iter().filter(|s| selected.is_empty() || selected.iter().any(|x| x == *s)) {
        eprintln!("--- stage {stage}");
        match *stage {
            "schema" => {
                secrets(&root)?;
                mirrors(&root)?;
                run(&root, "cargo", &["run", "-q", "-p", "contextful-spec", "--", "lint"])?
            }
            "test-first" => {
                provision_lean(&root)?;
                test_first(&root, base)?
            }
            "workspace" => {
                provision_lean(&root)?;
                workspace(&root)?
            }
            "acceptance" => acceptance(&root)?,
            _ => unreachable!(),
        }
    }
    Ok(())
}

fn workspace(root: &Path) -> Result<()> {
    let mut args = vec!["test", "--workspace"];
    if root.join(ACCEPTANCE_DIR).join("Cargo.toml").exists() {
        args.extend(["--exclude", ACCEPTANCE_PACKAGE]);
    }
    run(root, "cargo", &args)
}

// ---------------------------------------------------------------- lean

/// Install elan when absent and the toolchain `formal/lean-toolchain` pins, put elan's
/// `bin` first on `PATH`, pin `ELAN_TOOLCHAIN` to it, and set `CONTEXTFUL_REQUIRE_LEAN=1` for every test process
/// this gate run starts. A tree pinning no toolchain is left untouched.
fn provision_lean(root: &Path) -> Result<()> {
    let Ok(pin) = std::fs::read_to_string(root.join(LEAN_PIN)) else { return Ok(()) };
    let pin = pin.trim();
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
    run(root, "cargo", &["test", "-p", ACCEPTANCE_PACKAGE])
}

/// Package names of every workspace member, from `cargo metadata`.
fn workspace_packages(root: &Path) -> Result<Vec<String>> {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1", "-q"])
        .current_dir(root)
        .output()?;
    if !out.status.success() {
        bail!("cargo metadata: {}", String::from_utf8_lossy(&out.stderr));
    }
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).context("parsing cargo metadata")?;
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

fn test_first(root: &Path, base: &str) -> Result<()> {
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
    let verdict = red_against_base(root, &tree, &scratch.join("target"), &tests);
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

/// Overlay the change's test files on the base tree; return each package whose tests fail there.
fn red_against_base(root: &Path, tree: &Path, target: &Path, tests: &[&str]) -> Result<Vec<String>> {
    let mut packages: Vec<&str> = Vec::new();
    for t in tests {
        let dest = tree.join(t);
        std::fs::create_dir_all(dest.parent().unwrap())?;
        std::fs::copy(root.join(t), &dest).with_context(|| format!("overlaying {t}"))?;
        let pkg = t.split("/tests/").next().unwrap();
        if !packages.contains(&pkg) {
            packages.push(pkg);
        }
    }
    let mut red = Vec::new();
    for pkg in packages {
        let manifest = tree.join(pkg).join("Cargo.toml");
        if !manifest.exists() {
            red.push(format!("{pkg} (absent at base)"));
            continue;
        }
        let status = Command::new("cargo")
            .args(["test", "-q", "--tests", "--manifest-path"])
            .arg(&manifest)
            .env("CARGO_TARGET_DIR", target)
            .current_dir(tree)
            .status()?;
        if !status.success() {
            red.push(pkg.to_string());
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
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Paths git tracks under `root`.
fn tracked(root: &Path) -> Result<Vec<String>> {
    let out = Command::new("git").args(["ls-files", "-z"]).current_dir(root).output().context("running git")?;
    if !out.status.success() {
        bail!("git ls-files: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect())
}

fn run(root: &Path, program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program).args(args).current_dir(root).status()?;
    if !status.success() {
        bail!("`{program} {}` exited {}", args.join(" "), status.code().unwrap_or(-1));
    }
    Ok(())
}
