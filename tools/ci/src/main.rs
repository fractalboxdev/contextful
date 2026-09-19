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
    };
    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}

fn gate(selected: &[String], base: &str) -> Result<()> {
    let root = PathBuf::from(git(&["rev-parse", "--show-toplevel"])?);
    for stage in STAGES.iter().filter(|s| selected.is_empty() || selected.iter().any(|x| x == *s)) {
        eprintln!("--- stage {stage}");
        match *stage {
            "schema" => run(&root, "cargo", &["run", "-q", "-p", "contextful-spec", "--", "lint"])?,
            "test-first" => test_first(&root, base)?,
            "workspace" => workspace(&root)?,
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
    let changed = git(&["diff", "--name-only", "--diff-filter=ACDMR", &range])?;
    let sources: Vec<&str> = changed.lines().filter(|p| is_source(p)).collect();
    if sources.is_empty() {
        eprintln!("test-first: no Rust source under crates/ or tools/ changed");
        return Ok(());
    }
    let trailers = git(&["log", "--format=%(trailers:key=Test-First,valueonly)", &format!("{base}..HEAD")])?;
    if trailers.lines().any(|l| l.trim() == REFACTOR_TRAILER) {
        eprintln!("test-first: `Test-First: {REFACTOR_TRAILER}` — the workspace stage holds this range");
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

fn merge_base(base: &str) -> Result<String> {
    git(&["merge-base", base, "HEAD"])
}

fn is_source(p: &str) -> bool {
    (p.starts_with("crates/") || p.starts_with("tools/")) && p.ends_with(".rs") && p.contains("/src/")
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

fn run(root: &Path, program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program).args(args).current_dir(root).status()?;
    if !status.success() {
        bail!("`{program} {}` exited {}", args.join(" "), status.code().unwrap_or(-1));
    }
    Ok(())
}
