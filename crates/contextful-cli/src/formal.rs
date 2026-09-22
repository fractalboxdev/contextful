//! `contextful formal` — elaboration and axiom audit of the Lean models.
//!
//! `check` refuses a package manifest declaring a dependency and a toolchain that drifts
//! from its pin, refuses an inventory that publishes a theorem without its negative space,
//! names an unbound proof target or rests on an unnamed component, then builds the package
//! and reads each required constant's elaborated statement and transitive axiom footprint
//! off the elaborated environment through a generated Lean file. Source text decides
//! nothing. `recheck` repeats the audit over the commit's own source in an empty artifact
//! directory, from a credential-free environment, and compares the per-constant reports.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// The axioms a required constant's footprint may reach (`assurance-axiom-allowlist`: 2 entries).
pub const AXIOM_ALLOWLIST: [&str; 2] = ["propext", "Quot.sound"];

/// The hole axiom every `sorry`, however spelled, elaborates to.
pub const HOLE_AXIOM: &str = "sorryAx";

/// Axioms trusting the compiler's evaluation. `native_decide` also mints one axiom per
/// declaration, whose name carries a `_native` component.
pub const NATIVE_AXIOMS: [&str; 3] = ["Lean.ofReduceBool", "Lean.ofReduceNat", "Lean.trustCompiler"];

/// One recheck from an empty artifact directory (`assurance-recheck-wall-time`: 600 s).
pub const RECHECK_WALL_TIME: Duration = Duration::from_secs(600);

/// Where `check` writes its report, relative to the package root, absent `--report`.
pub const DEFAULT_REPORT: &str = ".lake/contextful-report.json";

/// The refusals of `contextful formal`, one variant per error identifier of the
/// `assurance` contract it raises. `Display` begins with the identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormalError {
    /// (`assurance.model.declared-dependency`)
    #[error("FormalPackageDependency: {0}")]
    FormalPackageDependency(String),
    /// (`assurance.model.toolchain-drift`)
    #[error("ProofToolchainDrift: {0}")]
    ProofToolchainDrift(String),
    /// (`assurance.prove.unbound-target`)
    #[error("ProofTargetUnbound: {0}")]
    ProofTargetUnbound(String),
    /// (`assurance.prove.no-negative-space`)
    #[error("TheoremWithoutNegativeSpace: {0}")]
    TheoremWithoutNegativeSpace(String),
    /// (`assurance.scope-claim.unnamed-dependency`)
    #[error("TrustedDependencyUnnamed: {0}")]
    TrustedDependencyUnnamed(String),
    /// (`assurance.audit-axioms.missing-constant`)
    #[error("TheoremConstantMissing: {0}")]
    TheoremConstantMissing(String),
    /// (`assurance.audit-axioms.statement-drift`)
    #[error("TheoremStatementDrift: {0}")]
    TheoremStatementDrift(String),
    /// (`assurance.audit-axioms.hole-axiom`)
    #[error("ProofHoleAxiom: {0}")]
    ProofHoleAxiom(String),
    /// (`assurance.audit-axioms.native-evaluation-axiom`)
    #[error("NativeEvaluationAxiom: {0}")]
    NativeEvaluationAxiom(String),
    /// (`assurance.audit-axioms.axiom-outside-allowlist`)
    #[error("AxiomOutsideAllowlist: {0}")]
    AxiomOutsideAllowlist(String),
    /// (`assurance.recheck.credential-free`)
    #[error("RecheckEnvironmentCredentialed: {0}")]
    RecheckEnvironmentCredentialed(String),
    /// (`assurance.recheck.report-mismatch`)
    #[error("RecheckReportMismatch: {0}")]
    RecheckReportMismatch(String),
}

impl FormalError {
    fn identifier(&self) -> String {
        self.to_string().split(':').next().unwrap_or_default().to_string()
    }
}

#[derive(Subcommand)]
pub enum FormalCmd {
    /// Elaborate the package, match every inventory row, audit every axiom footprint and
    /// write the report; exits non-zero naming the first failing constant.
    Check {
        /// The Lean package root (default: `formal/` under the repository).
        #[arg(long)]
        root: Option<PathBuf>,
        /// Where the report lands (default: `<root>/.lake/contextful-report.json`).
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Rebuild the commit's own source into an empty artifact directory from a
    /// credential-free environment, re-run the audit and compare per-constant reports.
    Recheck {
        /// The Lean package root (default: `formal/` under the repository).
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Replay the counterexample corpus, then run seeded generated cases through the engine's
    /// decision functions and the Lean reference model; exits non-zero on the first
    /// disagreement.
    Differential(crate::differential::DifferentialArgs),
}

pub fn run(cmd: FormalCmd) -> Result<()> {
    match cmd {
        FormalCmd::Check { root, report } => {
            let root = resolve_root(root)?;
            let report_path = report.unwrap_or_else(|| root.join(DEFAULT_REPORT));
            check(&root, &report_path)
        }
        FormalCmd::Recheck { root } => recheck(root),
        FormalCmd::Differential(args) => crate::differential::run(args),
    }
}

fn resolve_root(root: Option<PathBuf>) -> Result<PathBuf> {
    let root = match root {
        Some(r) => r,
        None => {
            let cwd = std::env::current_dir()?;
            git_toplevel(&cwd).unwrap_or(cwd).join("formal")
        }
    };
    if !root.is_dir() {
        bail!("no Lean package at {}", root.display());
    }
    Ok(root.canonicalize()?)
}

// ---------------------------------------------------------------- check

fn check(root: &Path, report_path: &Path) -> Result<()> {
    let report = audit(root)?;
    write_report(&report, report_path)?;
    for row in &report.constants {
        println!("{:<24} {}", row.verdict, row.name);
    }
    match first_failure(&report) {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

fn write_report(report: &Report, path: &Path) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(report)? + "\n")
        .with_context(|| format!("writing {}", path.display()))
}

fn first_failure(report: &Report) -> Option<FormalError> {
    report.constants.iter().find_map(|row| row.failure.clone())
}

/// The report `check` writes and `recheck` compares.
#[derive(Debug, Serialize)]
pub struct Report {
    pub commit: String,
    pub worktree_clean: bool,
    pub toolchain: Toolchain,
    pub allowlist: Vec<String>,
    pub inventory_revision: String,
    pub constants: Vec<Row>,
}

#[derive(Debug, Serialize)]
pub struct Toolchain {
    pub pinned: String,
    pub resolved: String,
}

#[derive(Debug, Serialize)]
pub struct Row {
    pub name: String,
    pub module: String,
    pub present: bool,
    pub statement_expected: String,
    pub statement_elaborated: Option<String>,
    pub statement_match: bool,
    /// The transitive footprint, sorted.
    pub axioms: Vec<String>,
    /// The axioms this row admits: its own list within the allowlist.
    pub admitted: Vec<String>,
    /// `ok`, or the identifier of the refusal the row raises.
    pub verdict: String,
    #[serde(skip)]
    pub failure: Option<FormalError>,
}

fn audit(root: &Path) -> Result<Report> {
    refuse_dependencies(root)?;
    let inventory = Inventory::load(root)?;
    inventory.refuse_unpublishable()?;
    let toolchain = resolve_toolchain(root)?;

    let build = Command::new("lake").arg("build").current_dir(root).output().context("running `lake build`")?;
    if !build.status.success() {
        // The exit status decides nothing: a failed module leaves its constants absent.
        eprintln!("note: `lake build` exited {}; auditing the environment it left", build.status);
    }

    let elaborated = elaborate(root, &inventory)?;
    let constants = inventory.rows.iter().map(|row| judge(row, elaborated.get(&row.name))).collect();
    Ok(Report {
        commit: git(root, &["rev-parse", "HEAD"]).unwrap_or_else(|| "none".into()),
        worktree_clean: git(root, &["status", "--porcelain", "--", "."]).is_some_and(|s| s.is_empty()),
        toolchain,
        allowlist: AXIOM_ALLOWLIST.iter().map(|s| s.to_string()).collect(),
        inventory_revision: inventory.revision,
        constants,
    })
}

fn judge(row: &InventoryRow, found: Option<&Elaborated>) -> Row {
    let admitted: Vec<String> = match &row.axioms {
        Some(list) => AXIOM_ALLOWLIST.iter().filter(|a| list.iter().any(|l| l == *a)).map(|a| a.to_string()).collect(),
        None => AXIOM_ALLOWLIST.iter().map(|a| a.to_string()).collect(),
    };
    let mut out = Row {
        name: row.name.clone(),
        module: row.module.clone(),
        present: found.is_some_and(|f| f.module == row.module),
        statement_expected: row.statement.clone(),
        statement_elaborated: found.map(|f| f.statement.clone()),
        statement_match: false,
        axioms: found.map(|f| f.axioms.iter().cloned().collect()).unwrap_or_default(),
        admitted,
        verdict: "ok".into(),
        failure: None,
    };
    let name = &row.name;
    let failure = match found {
        None => Some(FormalError::TheoremConstantMissing(format!(
            "`{name}` (module `{}`) is absent from the elaborated environment",
            row.module
        ))),
        // The row names the module the constant lives in; one found elsewhere is absent
        // from the module the inventory requires.
        Some(f) if f.module != row.module => Some(FormalError::TheoremConstantMissing(format!(
            "`{name}` is absent from module `{}`; the environment declares it in `{}`",
            row.module, f.module
        ))),
        Some(f) => {
            out.statement_match = normalize(&f.statement) == normalize(&row.statement);
            let native: Vec<&String> = f.axioms.iter().filter(|a| is_native(a)).collect();
            let outside: Vec<&String> =
                f.axioms.iter().filter(|a| !out.admitted.iter().any(|ok| ok == *a)).collect();
            if !out.statement_match {
                Some(FormalError::TheoremStatementDrift(format!(
                    "`{name}`: expected `{}`, elaborated `{}`",
                    row.statement, f.statement
                )))
            } else if f.axioms.contains(HOLE_AXIOM) {
                Some(FormalError::ProofHoleAxiom(format!("`{name}` reaches `{HOLE_AXIOM}`")))
            } else if let Some(ax) = native.first() {
                Some(FormalError::NativeEvaluationAxiom(format!(
                    "`{name}` reaches `{ax}`, minted by `{}`",
                    minter(ax).unwrap_or(name)
                )))
            } else if !outside.is_empty() {
                let list: Vec<&str> = outside.iter().map(|s| s.as_str()).collect();
                Some(FormalError::AxiomOutsideAllowlist(format!(
                    "`{name}` reaches {} (admitted: {})",
                    list.iter().map(|a| format!("`{a}`")).collect::<Vec<_>>().join(", "),
                    out.admitted.join(", ")
                )))
            } else {
                None
            }
        }
    };
    if let Some(e) = &failure {
        out.verdict = e.identifier();
    }
    out.failure = failure;
    out
}

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_native(axiom: &str) -> bool {
    NATIVE_AXIOMS.contains(&axiom) || axiom.split('.').any(|c| c == "_native")
}

/// The declaration a per-declaration native axiom was minted for: the name before its
/// `_native` component.
fn minter(axiom: &str) -> Option<&str> {
    axiom.find("._native").map(|i| &axiom[..i])
}

// ---------------------------------------------------------------- manifest and toolchain

fn refuse_dependencies(root: &Path) -> Result<()> {
    let path = root.join("lakefile.toml");
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let manifest: toml::Table = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if let Some(require) = manifest.get("require") {
        let names: Vec<String> = match require {
            toml::Value::Array(items) => items
                .iter()
                .map(|i| i.get("name").and_then(|n| n.as_str()).unwrap_or("<unnamed>").to_string())
                .collect(),
            _ => vec!["<unnamed>".into()],
        };
        return Err(FormalError::FormalPackageDependency(format!(
            "{} requires {}",
            path.display(),
            names.join(", ")
        ))
        .into());
    }
    Ok(())
}

fn resolve_toolchain(root: &Path) -> Result<Toolchain> {
    let pin_path = root.join("lean-toolchain");
    let pinned = std::fs::read_to_string(&pin_path)
        .with_context(|| format!("reading {}", pin_path.display()))?
        .trim()
        .to_string();
    let candidate = std::env::var("ELAN_TOOLCHAIN").ok().filter(|t| !t.is_empty()).unwrap_or_else(|| pinned.clone());
    // Resolving an uninstalled toolchain would download it; the check fetches nothing.
    if let Ok(out) = Command::new("elan").args(["toolchain", "list"]).output() {
        let listed = String::from_utf8_lossy(&out.stdout);
        if !listed.lines().any(|l| l.split_whitespace().next() == Some(candidate.as_str())) {
            bail!("toolchain `{candidate}` is not installed, and the check fetches nothing: `elan toolchain install {candidate}`");
        }
    }
    let out = Command::new("lean").arg("--version").current_dir(root).output().context("running `lean --version`")?;
    let resolved = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let resolved_version = resolved.split("version ").nth(1).and_then(|v| v.split([',', ' ']).next()).unwrap_or("");
    let pinned_version = pinned.rsplit(':').next().unwrap_or("").trim_start_matches('v');
    if !out.status.success() || resolved_version.is_empty() || resolved_version != pinned_version {
        return Err(FormalError::ProofToolchainDrift(format!("pinned `{pinned}`, resolved `{resolved}`")).into());
    }
    Ok(Toolchain { pinned, resolved })
}

// ---------------------------------------------------------------- inventory

#[derive(Debug, Deserialize)]
struct RawRow {
    module: Option<String>,
    statement: Option<String>,
    axioms: Option<Vec<String>>,
    binding: Option<String>,
    negative: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Claim {
    #[serde(default)]
    targets: Vec<String>,
    #[serde(default)]
    trusted: Vec<String>,
    #[serde(default)]
    rests_on: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawInventory {
    #[serde(default)]
    claim: Claim,
    #[serde(default)]
    constant: BTreeMap<String, RawRow>,
}

#[derive(Debug)]
struct InventoryRow {
    name: String,
    module: String,
    statement: String,
    axioms: Option<Vec<String>>,
    binding: Option<String>,
    negative: Option<String>,
}

/// `inventory.toml`: one row per required constant, in file order, and the claim.
#[derive(Debug)]
struct Inventory {
    revision: String,
    claim: Claim,
    rows: Vec<InventoryRow>,
}

impl Inventory {
    fn load(root: &Path) -> Result<Inventory> {
        let path = root.join("inventory.toml");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut raw: RawInventory = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let doc: toml_edit::DocumentMut = text.parse()?;
        let order: Vec<String> = doc
            .get("constant")
            .and_then(|c| c.as_table_like())
            .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
            .unwrap_or_default();
        let mut rows = Vec::new();
        for name in order {
            let r = raw.constant.remove(&name).expect("toml and toml_edit agree on keys");
            let module = r.module.with_context(|| format!("inventory row `{name}` names no module"))?;
            let statement = r.statement.with_context(|| format!("inventory row `{name}` states no statement"))?;
            rows.push(InventoryRow {
                name,
                module,
                statement,
                axioms: r.axioms,
                binding: r.binding,
                negative: r.negative,
            });
        }
        let revision = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
        Ok(Inventory { revision, claim: raw.claim, rows })
    }

    /// The refusals the inventory's text alone decides, before anything builds.
    fn refuse_unpublishable(&self) -> Result<()> {
        let blank = |s: &Option<String>| s.as_deref().is_none_or(|s| s.trim().is_empty());
        if let Some(row) = self.rows.iter().find(|r| blank(&r.negative)) {
            return Err(FormalError::TheoremWithoutNegativeSpace(format!(
                "`{}` states nothing it leaves open",
                row.name
            ))
            .into());
        }
        for target in &self.claim.targets {
            match self.rows.iter().find(|r| &r.name == target) {
                Some(row) if !blank(&row.binding) => {}
                Some(_) => {
                    return Err(FormalError::ProofTargetUnbound(format!(
                        "`{target}` carries no binding to a query path"
                    ))
                    .into())
                }
                None => {
                    return Err(FormalError::ProofTargetUnbound(format!(
                        "`{target}` is claimed and has no inventory row"
                    ))
                    .into())
                }
            }
        }
        let trusted: BTreeSet<&String> = self.claim.trusted.iter().collect();
        if let Some(c) = self.claim.rests_on.iter().find(|c| !trusted.contains(c)) {
            return Err(FormalError::TrustedDependencyUnnamed(format!(
                "the claim rests on `{c}`, absent from its trusted dependencies"
            ))
            .into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- the elaborated environment

#[derive(Debug, Deserialize)]
struct Elaborated {
    module: String,
    statement: String,
    axioms: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
struct ElaboratedRow {
    module: Option<String>,
    statement: Option<String>,
    axioms: Option<BTreeSet<String>>,
}

/// Reads each row's elaborated statement and transitive axiom footprint off the
/// environment `lake build` wrote, through a generated Lean file importing every row's
/// module that elaborated. A constant whose module left no `.olean` is absent.
fn elaborate(root: &Path, inventory: &Inventory) -> Result<BTreeMap<String, Elaborated>> {
    let modules: BTreeSet<&str> =
        inventory.rows.iter().map(|r| r.module.as_str()).filter(|m| olean_exists(root, m)).collect();
    let dir = root.join(".lake/contextful");
    std::fs::create_dir_all(&dir)?;
    let out_path = dir.join("elaborated.json");
    let _ = std::fs::remove_file(&out_path);

    let mut src = String::from("import Lean\n");
    for m in &modules {
        src.push_str(&format!("import {m}\n"));
    }
    let names: Vec<String> = inventory.rows.iter().map(|r| format!("{}.toName", lean_string(&r.name))).collect();
    src.push_str(&format!(
        r#"open Lean Meta

#eval show MetaM Unit from do
  let env ← getEnv
  let mut rows : Array Json := #[]
  for c in [{names}] do
    match env.find? c with
    | none => rows := rows.push (Json.mkObj [("name", toJson c.toString)])
    | some info =>
      let f ← ppExpr info.type
      let axs ← collectAxioms c
      let m := match env.getModuleIdxFor? c with
        | some i => (env.header.moduleNames[i.toNat]?).getD Name.anonymous
        | none => Name.anonymous
      rows := rows.push (Json.mkObj [("name", toJson c.toString),
        ("module", toJson m.toString),
        ("statement", toJson (f.pretty 1000000)),
        ("axioms", toJson (axs.map (·.toString)))])
  IO.FS.writeFile {out} (Json.arr rows).compress
"#,
        names = names.join(", "),
        out = lean_string(&out_path.to_string_lossy()),
    ));
    let file = dir.join("Audit.lean");
    std::fs::write(&file, src)?;

    let run = Command::new("lake")
        .args(["env", "lean"])
        .arg(&file)
        .current_dir(root)
        .output()
        .context("running `lake env lean`")?;
    let text = std::fs::read_to_string(&out_path).map_err(|_| {
        anyhow::anyhow!(
            "the audit file did not elaborate:\n{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        )
    })?;
    let rows: Vec<ElaboratedRow> = serde_json::from_str(&text)?;
    // `Name.toString` escapes a component needing guillemets; key by inventory spelling.
    Ok(inventory
        .rows
        .iter()
        .zip(rows)
        .filter_map(|(inv, row)| {
            let statement = row.statement?;
            let module = row.module.unwrap_or_default();
            Some((inv.name.clone(), Elaborated { module, statement, axioms: row.axioms.unwrap_or_default() }))
        })
        .collect())
}

fn olean_exists(root: &Path, module: &str) -> bool {
    let rel: PathBuf = module.split('.').collect::<PathBuf>().with_extension("olean");
    ["lib/lean", "lib"].iter().any(|d| root.join(".lake/build").join(d).join(&rel).is_file())
}

fn lean_string(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------- recheck

fn recheck(root: Option<PathBuf>) -> Result<()> {
    let started = Instant::now();
    refuse_credentials()?;
    // The recheck resolves the pinned toolchain, whatever override the caller carries.
    std::env::remove_var("ELAN_TOOLCHAIN");
    let root = resolve_root(root)?;

    let first = audit(&root)?;
    if let Some(e) = first_failure(&first) {
        return Err(e.into());
    }

    let top = git_toplevel(&root).context("a recheck reads the commit's source: the package is not in a git repository")?;
    let rel = root.strip_prefix(top.canonicalize()?).unwrap_or(Path::new("")).to_path_buf();
    let scratch = std::env::temp_dir().join(format!(
        "contextful-recheck-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()
    ));
    let rebuilt = scratch.join(&rel);
    export_commit(&top, &rel, &scratch)?;
    if rebuilt.join(".lake").exists() {
        bail!("the rebuild directory {} carries artifacts", rebuilt.display());
    }
    println!("rebuilt: {}", rebuilt.display());
    let second = audit(&rebuilt);
    let _ = std::fs::remove_dir_all(&scratch);
    let second = second?;
    if let Some(e) = first_failure(&second) {
        return Err(e.into());
    }

    let index = |r: &Report| -> BTreeMap<String, (Option<String>, Vec<String>)> {
        r.constants.iter().map(|c| (c.name.clone(), (c.statement_elaborated.clone(), c.axioms.clone()))).collect()
    };
    let (a, b) = (index(&first), index(&second));
    let names: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let differing: Vec<String> = names.into_iter().filter(|n| a.get(*n) != b.get(*n)).map(|n| format!("`{n}`")).collect();
    if !differing.is_empty() {
        return Err(FormalError::RecheckReportMismatch(format!(
            "{} differ between the working package and commit {}",
            differing.join(", "),
            second.commit
        ))
        .into());
    }
    if started.elapsed() > RECHECK_WALL_TIME {
        bail!("the recheck took {:?}, past its {:?} budget", started.elapsed(), RECHECK_WALL_TIME);
    }
    for row in &second.constants {
        println!("{:<24} {}", "match", row.name);
    }
    Ok(())
}

/// Writes every file of `HEAD` under `rel` into `dest`, keeping its path.
fn export_commit(top: &Path, rel: &Path, dest: &Path) -> Result<()> {
    let spec = if rel.as_os_str().is_empty() { ".".to_string() } else { rel.to_string_lossy().to_string() };
    let listing = Command::new("git")
        .args(["ls-tree", "-r", "-z", "--name-only", "HEAD", "--"])
        .arg(&spec)
        .current_dir(top)
        .output()?;
    if !listing.status.success() {
        bail!("git ls-tree: {}", String::from_utf8_lossy(&listing.stderr));
    }
    for path in listing.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8_lossy(path).to_string();
        let blob = Command::new("git").args(["cat-file", "blob", &format!("HEAD:{path}")]).current_dir(top).output()?;
        if !blob.status.success() {
            bail!("git cat-file {path}: {}", String::from_utf8_lossy(&blob.stderr));
        }
        let target = dest.join(&path);
        std::fs::create_dir_all(target.parent().expect("a file path has a parent"))?;
        std::fs::write(target, blob.stdout)?;
    }
    Ok(())
}

/// Environment variable names that carry a token, a signing key or a registry login.
fn credential_variable(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.ends_with("_TOKEN")
        || n == "TOKEN"
        || n.contains("SECRET")
        || n.contains("PASSWORD")
        || n.contains("PRIVATE_KEY")
        || n.contains("ACCESS_KEY")
        || n.contains("CREDENTIAL")
        || n.ends_with("_API_KEY")
        || (n.starts_with("AWS_") && n.contains("KEY"))
        || ["SSH_AUTH_SOCK", "GPG_AGENT_INFO", "DOCKER_AUTH_CONFIG"].contains(&n.as_str())
}

/// Files under the home directory that carry a login or a signing key.
fn credential_files(home: &Path, cargo_home: Option<&Path>) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let cargo = cargo_home.map(Path::to_path_buf).unwrap_or_else(|| home.join(".cargo"));
    let plain = [
        home.join(".git-credentials"),
        home.join(".netrc"),
        home.join(".aws/credentials"),
        home.join(".pypirc"),
        home.join(".config/gh/hosts.yml"),
        cargo.join("credentials"),
        cargo.join("credentials.toml"),
    ];
    found.extend(plain.into_iter().filter(|p| p.is_file()));
    for (file, marker) in [(home.join(".npmrc"), "_auth"), (home.join(".docker/config.json"), "\"auth\"")] {
        if std::fs::read_to_string(&file).is_ok_and(|t| t.contains(marker)) {
            found.push(file);
        }
    }
    if let Ok(entries) = std::fs::read_dir(home.join(".ssh")) {
        let mut keys: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                name.starts_with("id_") && !name.ends_with(".pub") && p.is_file()
            })
            .collect();
        keys.sort();
        found.extend(keys);
    }
    let gpg = home.join(".gnupg/private-keys-v1.d");
    if std::fs::read_dir(&gpg).is_ok_and(|mut d| d.next().is_some()) {
        found.push(gpg);
    }
    found
}

fn refuse_credentials() -> Result<()> {
    let mut carriers: Vec<String> = std::env::vars_os()
        .filter(|(k, v)| !v.is_empty() && credential_variable(&k.to_string_lossy()))
        .map(|(k, _)| format!("variable `{}`", k.to_string_lossy()))
        .collect();
    carriers.sort();
    if let Some(home) = std::env::var_os("HOME") {
        let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
        carriers.extend(
            credential_files(Path::new(&home), cargo_home.as_deref()).iter().map(|p| format!("file `{}`", p.display())),
        );
    }
    if carriers.is_empty() {
        Ok(())
    } else {
        Err(FormalError::RecheckEnvironmentCredentialed(carriers.join(", ")).into())
    }
}

// ---------------------------------------------------------------- git

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(dir).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_toplevel(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}
