//! `contextful formal check` and `contextful formal recheck` over fixture Lean packages.
//!
//! Each test copies `tests/fixtures/formal/base` into a temporary directory, pins it to an
//! installed toolchain, and drives the built binary. A test reaching Lean skips when no
//! `lean` is on `PATH`, unless `CONTEXTFUL_REQUIRE_LEAN` is set, in which case it fails.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_contextful");

fn fixture_base() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/formal/base")
}

fn elan_home() -> Option<PathBuf> {
    std::env::var_os("ELAN_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".elan")))
}

/// Toolchains elan has installed, as `leanprover/lean4:vX.Y.Z` strings.
fn installed_toolchains() -> Vec<String> {
    let Ok(out) = Command::new("elan").args(["toolchain", "list"]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|t| t.contains(':'))
        .map(str::to_string)
        .collect()
}

/// The installed toolchain the fixtures pin: the default one when it is listed, else
/// the last listed. `None` when Lean is absent.
fn toolchain() -> Option<String> {
    let installed = installed_toolchains();
    let out = Command::new("lean").arg("--version").current_dir(std::env::temp_dir()).output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout).to_string();
    let version = line.split("version ").nth(1)?.split([',', ' ']).next()?.to_string();
    let default = format!("leanprover/lean4:v{version}");
    if installed.contains(&default) {
        Some(default)
    } else {
        installed.last().cloned()
    }
}

macro_rules! lean_or_skip {
    () => {
        match toolchain() {
            Some(t) => t,
            None => {
                assert!(
                    std::env::var_os("CONTEXTFUL_REQUIRE_LEAN").is_none(),
                    "CONTEXTFUL_REQUIRE_LEAN is set and no Lean toolchain is installed"
                );
                eprintln!("skipped: no Lean toolchain on PATH");
                return;
            }
        }
    };
}

/// A fixture package inside a git repository at `<tmp>/repo/formal`, with a private
/// empty home directory.
struct Pkg {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    root: PathBuf,
    home: PathBuf,
}

impl Pkg {
    fn new(toolchain: &str) -> Pkg {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let root = repo.join("formal");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        copy_dir(&fixture_base(), &root);
        std::fs::write(root.join("lean-toolchain"), format!("{toolchain}\n")).unwrap();
        Pkg { _tmp: tmp, repo, root, home }
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    /// Appends one inventory row.
    fn require(&self, name: &str, statement: &str) {
        let mut inv = self.read("inventory.toml");
        inv.push_str(&format!(
            "\n[constant.\"{name}\"]\nmodule    = \"Fx.Basic\"\nstatement = \"{statement}\"\n\
             assumptions    = [\"propext\", \"Quot.sound\"]\nbinding   = \"engine/fx/src/lib.rs::x\"\n\
             negative  = \"Leaves open: the rest.\"\n"
        ));
        self.write("inventory.toml", &inv);
    }

    /// Adds Lean declarations to `Fx/Basic.lean`, inside the namespace.
    fn declare(&self, lean: &str) {
        let src = self.read("Fx/Basic.lean").replace("end Fx", &format!("{lean}\n\nend Fx"));
        self.write("Fx/Basic.lean", &src);
    }

    fn command(&self) -> Command {
        let mut c = Command::new(BIN);
        c.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .current_dir(&self.repo);
        if let Some(e) = elan_home() {
            c.env("ELAN_HOME", e);
        }
        c
    }

    fn check(&self) -> Output {
        self.command().args(["formal", "check", "--root"]).arg(&self.root).output().unwrap()
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn commit(&self) {
        std::fs::write(self.repo.join(".gitignore"), ".lake/\n").unwrap();
        self.git(&["init", "-q"]);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "fixture"]);
    }

    fn report(&self) -> serde_json::Value {
        let text = std::fs::read_to_string(self.root.join(".lake/contextful-report.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

fn passed(out: &Output) -> String {
    assert!(
        out.status.success(),
        "expected success\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn refused(out: &Output, error: &str) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "expected {error}, got success: {}", String::from_utf8_lossy(&out.stdout));
    assert!(stderr.contains(error), "expected {error}, got: {stderr}");
    stderr
}

/// `contextful formal check` elaborates the package, matches every inventory row, audits every footprint, writes the report, and exits non-zero naming the first failing constant.
// spec: assurance.audit-assumptions.check-command@bd29299d
#[test]
fn check_passes_a_clean_package_and_names_the_first_failing_constant() {
    let t = lean_or_skip!();
    let clean = Pkg::new(&t);
    let out = passed(&clean.check());
    assert!(out.contains("Fx.narrows") && out.contains("Fx.extensional"), "{out}");
    assert!(clean.root.join(".lake/contextful-report.json").is_file());

    let failing = Pkg::new(&t);
    failing.declare("theorem gap (n : Nat) : n = n := sorry\n\ntheorem gap2 (n : Nat) : n = n := sorry");
    failing.require("Fx.gap", "∀ (n : Nat), n = n");
    failing.require("Fx.gap2", "∀ (n : Nat), n = n");
    let err = refused(&failing.check(), "ProofHoleAssumption");
    let first = err.lines().last().unwrap();
    assert!(first.contains("Fx.gap") && !first.contains("Fx.gap2"), "{err}");
    assert_eq!(failing.report()["constants"].as_array().unwrap().len(), 4, "every row is reported");
}

/// The assumption allowlist holds 2 entries, `propext` and `Quot.sound`, and every theorem's footprint is a subset of it.
// spec: assurance.audit-assumptions.allowlist@2a9cf235
#[test]
fn the_allowlist_is_propext_and_quot_sound() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    passed(&p.check());
    let report = p.report();
    assert_eq!(report["allowlist"], serde_json::json!(["propext", "Quot.sound"]));
    let ext = report["constants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Fx.extensional")
        .unwrap()
        .clone();
    assert_eq!(ext["assumptions"], serde_json::json!(["Quot.sound", "propext"]));
}

/// Each required constant's assumption footprint is read transitively off the elaborated environment, naming every assumption reached.
// spec: assurance.audit-assumptions.transitive-audit@12964479
#[test]
fn a_footprint_reaches_through_helper_lemmas() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    p.declare("theorem helper (p : Prop) : p ∨ ¬p := Classical.em p\n\ntheorem viaHelper (p : Prop) : p ∨ ¬p := helper p");
    p.require("Fx.viaHelper", "∀ (p : Prop), p ∨ ¬p");
    let err = refused(&p.check(), "AssumptionOutsideAllowlist");
    assert!(err.contains("Fx.viaHelper") && err.contains("Classical.choice"), "{err}");
    let row = p.report()["constants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Fx.viaHelper")
        .unwrap()
        .clone();
    assert_eq!(row["assumptions"], serde_json::json!(["Classical.choice", "Quot.sound", "propext"]));
}

/// A verdict is a function of the elaborated environment and the inventory alone; source text, declaration counts and the build's exit status decide nothing.
// spec: assurance.audit-assumptions.verdict-input@082a7987
#[test]
fn source_text_and_build_status_decide_nothing() {
    let t = lean_or_skip!();
    // The word `sorry` in a comment and a string is no hole.
    let p = Pkg::new(&t);
    p.declare("-- sorry, admit\ndef note : String := \"sorry\"");
    passed(&p.check());

    // A build failing in a module no required constant lives in still audits the rows.
    let q = Pkg::new(&t);
    q.write("Fx.lean", "import Fx.Basic\nimport Fx.Broken\n");
    q.write("Fx/Broken.lean", "theorem broken : 1 = 2 := rfl\n");
    passed(&q.check());
}

/// An assumption in a footprint and absent from the allowlist raises `AssumptionOutsideAllowlist`, printing the constant and the assumption.
// spec: assurance.audit-assumptions.assumption-outside-allowlist@2e71d5b7
#[test]
fn classical_choice_is_outside_the_allowlist() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    p.declare("theorem em' (p : Prop) : p ∨ ¬p := Classical.em p");
    p.require("Fx.em'", "∀ (p : Prop), p ∨ ¬p");
    let err = refused(&p.check(), "AssumptionOutsideAllowlist");
    assert!(err.contains("Fx.em'") && err.contains("Classical.choice"), "{err}");
}

/// A footprint containing the hole assumption raises `ProofHoleAssumption`, naming the declaration, whatever its syntax spells.
// spec: assurance.audit-assumptions.hole-assumption@48ca973f
#[test]
fn a_parenthesized_hole_is_a_hole() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    p.declare("theorem holey (n : Nat) : n = n := by exact (sorry)");
    p.require("Fx.holey", "∀ (n : Nat), n = n");
    let err = refused(&p.check(), "ProofHoleAssumption");
    assert!(err.contains("Fx.holey"), "{err}");
}

/// A footprint containing a per-declaration native-evaluation assumption raises `NativeEvaluationAssumption`, naming the declaration that minted it.
// spec: assurance.audit-assumptions.native-evaluation-assumption@52b00896
#[test]
fn native_decide_mints_a_native_evaluation_assumption() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    p.declare("theorem sums : 10 + 10 = 20 := by native_decide");
    p.require("Fx.sums", "10 + 10 = 20");
    let err = refused(&p.check(), "NativeEvaluationAssumption");
    assert!(err.contains("Fx.sums"), "{err}");
}

/// A required constant absent from the elaborated environment raises `TheoremConstantMissing`.
// spec: assurance.audit-assumptions.missing-constant@539ef556
#[test]
fn a_deleted_theorem_is_missing() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    p.require("Fx.deleted", "∀ (n : Nat), n = n");
    let err = refused(&p.check(), "TheoremConstantMissing");
    assert!(err.contains("Fx.deleted"), "{err}");

    // A row naming a module that does not exist is missing too, not a crash.
    let q = Pkg::new(&t);
    let inv = q.read("inventory.toml").replace("module    = \"Fx.Basic\"\nstatement = \"∀ (n : Nat), n + 0 = n\"", "module    = \"Fx.Gone\"\nstatement = \"∀ (n : Nat), n + 0 = n\"");
    q.write("inventory.toml", &inv);
    let err = refused(&q.check(), "TheoremConstantMissing");
    assert!(err.contains("Fx.narrows"), "{err}");
}

/// A required constant whose elaborated statement differs from its expected text raises `TheoremStatementDrift`, printing both.
// spec: assurance.audit-assumptions.statement-drift@c254061a
#[test]
fn a_weakened_statement_drifts() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    let src = p.read("Fx/Basic.lean").replace("theorem narrows (n : Nat) : n + 0 = n := rfl", "theorem narrows (n : Nat) : n = n := rfl");
    p.write("Fx/Basic.lean", &src);
    let err = refused(&p.check(), "TheoremStatementDrift");
    assert!(err.contains("Fx.narrows"), "{err}");
    assert!(err.contains("∀ (n : Nat), n + 0 = n") && err.contains("∀ (n : Nat), n = n"), "{err}");
}

/// The report names the commit, the resolved toolchain, the allowlist and inventory revision applied, and for each required constant its statement match and the assumptions it reaches.
// spec: assurance.audit-assumptions.report@6e57548d
#[test]
fn the_report_names_commit_toolchain_allowlist_revision_and_rows() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    p.commit();
    let out = p.command().args(["formal", "check", "--root", "formal", "--report", "audit.json"]).output().unwrap();
    passed(&out);
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.repo.join("audit.json")).unwrap()).unwrap();
    let head = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&p.repo).output().unwrap();
    assert_eq!(report["commit"], String::from_utf8_lossy(&head.stdout).trim());
    assert_eq!(report["toolchain"]["pinned"], t.as_str());
    let version = t.rsplit(":v").next().unwrap();
    assert!(report["toolchain"]["resolved"].as_str().unwrap().contains(version), "{report}");
    assert_eq!(report["allowlist"], serde_json::json!(["propext", "Quot.sound"]));
    assert_eq!(report["inventory_revision"].as_str().unwrap().len(), "sha256:".len() + 64);
    let rows = report["constants"].as_array().unwrap();
    assert_eq!(rows[0]["name"], "Fx.narrows");
    assert_eq!(rows[0]["statement_match"], true);
    assert_eq!(rows[0]["assumptions"], serde_json::json!([]));
    assert_eq!(rows[1]["name"], "Fx.extensional");
}

/// A `require` stanza in the policy package's `lakefile.toml` reaching any external library raises `FormalPackageDependency`, naming the library.
// spec: assurance.model.declared-dependency@1c68e5d8
#[test]
fn a_require_stanza_is_refused() {
    let p = Pkg::new("leanprover/lean4:v4.0.0");
    let lakefile = p.read("lakefile.toml");
    p.write(
        "lakefile.toml",
        &format!("{lakefile}\n[[require]]\nname = \"mathlib\"\nscope = \"leanprover-community\"\n"),
    );
    let err = refused(&p.check(), "FormalPackageDependency");
    assert!(err.contains("mathlib"), "{err}");
}

/// A build whose resolved toolchain string differs from the pinned one raises `ProofToolchainDrift`, printing both strings.
// spec: assurance.model.toolchain-drift@6a88d6a0
#[test]
fn an_override_toolchain_drifts_from_the_pin() {
    let t = lean_or_skip!();
    let Some(other) = installed_toolchains().into_iter().find(|o| *o != t) else {
        eprintln!("skipped: a second installed toolchain is needed");
        return;
    };
    let p = Pkg::new(&t);
    let out = p.command().env("ELAN_TOOLCHAIN", &other).args(["formal", "check", "--root"]).arg(&p.root).output().unwrap();
    let err = refused(&out, "ProofToolchainDrift");
    let other_version = other.rsplit(":v").next().unwrap();
    assert!(err.contains(&t) && err.contains(other_version), "{err}");
    assert!(!p.root.join(".lake/build").exists(), "nothing builds under a drifted toolchain");
}

/// A target the claim names with no binding raises `ProofTargetUnbound`, naming the target.
// spec: assurance.prove.unbound-target@a4fa630c
#[test]
fn a_claimed_target_without_a_binding_is_refused() {
    let p = Pkg::new("leanprover/lean4:v4.0.0");
    let inv = p.read("inventory.toml").replace("binding   = \"engine/fx/src/lib.rs::narrows\"\n", "");
    p.write("inventory.toml", &inv);
    let err = refused(&p.check(), "ProofTargetUnbound");
    assert!(err.contains("Fx.narrows"), "{err}");

    // A claimed target with no inventory row is unbound too.
    let q = Pkg::new("leanprover/lean4:v4.0.0");
    let inv = q.read("inventory.toml").replace("targets  = [\"Fx.narrows\"]", "targets  = [\"Fx.narrows\", \"Fx.mediates\"]");
    q.write("inventory.toml", &inv);
    let err = refused(&q.check(), "ProofTargetUnbound");
    assert!(err.contains("Fx.mediates"), "{err}");
}

/// A theorem published without its negative space raises `TheoremWithoutNegativeSpace`, naming the constant.
// spec: assurance.prove.no-negative-space@bcd931d6
#[test]
fn a_row_without_negative_space_is_refused() {
    let p = Pkg::new("leanprover/lean4:v4.0.0");
    let inv = p.read("inventory.toml").replace("negative  = \"Leaves open: every function outside Nat.\"\n", "negative  = \"  \"\n");
    p.write("inventory.toml", &inv);
    let err = refused(&p.check(), "TheoremWithoutNegativeSpace");
    assert!(err.contains("Fx.extensional"), "{err}");
}

/// A claim resting on a component absent from its trusted-dependency list raises `TrustedDependencyUnnamed`, naming the component.
// spec: assurance.scope-claim.unnamed-dependency@11faa519
#[test]
fn a_claim_resting_on_an_unnamed_component_is_refused() {
    let p = Pkg::new("leanprover/lean4:v4.0.0");
    let inv = p.read("inventory.toml").replace("rests_on = [\"delegation-library\"]", "rests_on = [\"delegation-library\", \"sql-parser\"]");
    p.write("inventory.toml", &inv);
    let err = refused(&p.check(), "TrustedDependencyUnnamed");
    assert!(err.contains("sql-parser"), "{err}");
}

/// A recheck environment exposing a token, a signing key or a registry login raises `RecheckEnvironmentCredentialed`, naming the variable or file carrying it.
// spec: assurance.recheck.credential-free@2d8c567f
#[test]
fn a_credentialed_recheck_is_refused() {
    let p = Pkg::new("leanprover/lean4:v4.0.0");
    let out = p.command().env("GITHUB_TOKEN", "ghs_x").args(["formal", "recheck"]).output().unwrap();
    let err = refused(&out, "RecheckEnvironmentCredentialed");
    assert!(err.contains("GITHUB_TOKEN") && !err.contains("ghs_x"), "{err}");

    let out = p.command().env("CARGO_REGISTRY_TOKEN", "x").args(["formal", "recheck"]).output().unwrap();
    assert!(refused(&out, "RecheckEnvironmentCredentialed").contains("CARGO_REGISTRY_TOKEN"));

    std::fs::write(p.home.join(".git-credentials"), "https://u:p@example.invalid\n").unwrap();
    let out = p.command().args(["formal", "recheck"]).output().unwrap();
    assert!(refused(&out, "RecheckEnvironmentCredentialed").contains(".git-credentials"));
    std::fs::remove_file(p.home.join(".git-credentials")).unwrap();

    std::fs::create_dir_all(p.home.join(".ssh")).unwrap();
    std::fs::write(p.home.join(".ssh/id_ed25519"), "-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();
    let out = p.command().args(["formal", "recheck"]).output().unwrap();
    assert!(refused(&out, "RecheckEnvironmentCredentialed").contains("id_ed25519"));
}

/// A recheck rebuilds the package from the commit's own source and pinned toolchain into an empty artifact directory, fetching nothing, and re-runs the audit against the rebuilt environment.
// spec: assurance.recheck.two-phase@fdf78823
#[test]
fn a_recheck_rebuilds_the_commit_and_reaches_the_same_report() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    p.commit();
    let out = passed(&p.command().args(["formal", "recheck"]).output().unwrap());
    assert!(out.contains("Fx.narrows") && out.contains("Fx.extensional"), "{out}");
    let rebuilt = out.lines().find_map(|l| l.strip_prefix("rebuilt: ")).expect("names the rebuild directory");
    assert!(!Path::new(rebuilt).starts_with(&p.root), "{rebuilt}");

    // An untracked module the working tree imports is absent from the rebuild: the second
    // phase reads the commit, so its report lacks the row the first phase audited.
    let q = Pkg::new(&t);
    q.commit();
    q.write("Fx/Fresh.lean", "namespace Fx\ntheorem fresh (n : Nat) : n = n := rfl\nend Fx\n");
    q.write("Fx.lean", "import Fx.Basic\nimport Fx.Fresh\n");
    q.require("Fx.fresh", "∀ (n : Nat), n = n");
    let inv = q.read("inventory.toml").replace("[constant.\"Fx.fresh\"]\nmodule    = \"Fx.Basic\"", "[constant.\"Fx.fresh\"]\nmodule    = \"Fx.Fresh\"");
    q.write("inventory.toml", &inv);
    passed(&q.check());
    let err = refused(&q.command().args(["formal", "recheck"]).output().unwrap(), "RecheckReportMismatch");
    assert!(err.contains("Fx.fresh"), "{err}");
}

/// A recheck whose per-constant statement text or assumption set differs from the first phase's raises `RecheckReportMismatch`, naming the constant.
// spec: assurance.recheck.report-mismatch@80ed8394
#[test]
fn a_recheck_disagreeing_with_the_first_phase_is_refused() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    p.commit();
    // The working tree proves `narrows` through `propext`; the commit proves it by `rfl`.
    let src = p.read("Fx/Basic.lean").replace(
        "theorem narrows (n : Nat) : n + 0 = n := rfl",
        "theorem narrows (n : Nat) : n + 0 = n := by\n  have _h := propext (Iff.intro (id : True → True) id)\n  rfl",
    );
    p.write("Fx/Basic.lean", &src);
    let err = refused(&p.command().args(["formal", "recheck"]).output().unwrap(), "RecheckReportMismatch");
    assert!(err.contains("Fx.narrows") && !err.contains("Fx.extensional"), "{err}");
}
