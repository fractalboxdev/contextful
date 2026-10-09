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
        let pkg = Pkg { _tmp: tmp, repo, root, home };
        // The engine code the fixture's targets bind to.
        pkg.write_tree("engine/fx/src/lib.rs", "pub fn narrows(n: u64) -> u64 {\n    n\n}\n\npub fn extensional() {}\n");
        pkg.write_tree(
            "engine/fx/tests/fx.rs",
            "#[test]\nfn narrows_holds() {\n    assert_eq!(fx::narrows(1), 1);\n}\n\n#[test]\nfn extensional_holds() {}\n",
        );
        pkg
    }

    /// Writes a file relative to the repository rather than the package.
    fn write_tree(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// Adds `fields` (TOML lines) to the inventory row `name`.
    fn amend(&self, name: &str, fields: &str) {
        let inv = self.read("inventory.toml");
        let header = format!("[constant.\"{name}\"]\n");
        assert!(inv.contains(&header), "no row {name}");
        self.write("inventory.toml", &inv.replace(&header, &format!("{header}{fields}\n")));
    }

    /// Adds `fields` (TOML lines) to the `[claim]` table.
    fn claim(&self, fields: &str) {
        let inv = self.read("inventory.toml").replace("[claim]\n", &format!("[claim]\n{fields}\n"));
        self.write("inventory.toml", &inv);
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
    let inv = p
        .read("inventory.toml")
        .replace("binding   = \"engine/fx/src/lib.rs::narrows; test engine/fx/tests/fx.rs::narrows_holds\"\n", "");
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

// ---------------------------------------------------------------- the claim's reach

/// A claim that the enforcement stages commute raises `CommutationClaimed`.
// spec: assurance.prove.commutation-claim@545e6b8c
#[test]
fn a_commutation_claim_is_refused() {
    let p = Pkg::new("leanprover/lean4:v4.0.0");
    p.amend("Fx.narrows", "offered_as = \"commutation\"");
    let err = refused(&p.check(), "CommutationClaimed");
    assert!(err.contains("Fx.narrows"), "{err}");
}

/// Inclusion decided by evaluating a fixed set of example values raises `ZoneInclusionSampled`.
// spec: assurance.prove.sampled-inclusion@78ae6beb
#[test]
fn inclusion_over_sampled_placements_is_refused() {
    // A bounded quantifier over a fixed list of placements is a sample.
    let p = Pkg::new("leanprover/lean4:v4.0.0");
    p.require("Fx.sampled", "∀ p ∈ [Fx.Placement.a, Fx.Placement.b], Fx.small p = true → Fx.big p = true");
    p.amend("Fx.sampled", "offered_as = \"zone-inclusion\"");
    let err = refused(&p.check(), "ZoneInclusionSampled");
    assert!(err.contains("Fx.sampled"), "{err}");

    // The elaborator spells that bound as a membership hypothesis on the binder.
    let e = Pkg::new("leanprover/lean4:v4.0.0");
    e.require("Fx.bounded", "∀ (p : Fx.Placement), p ∈ Fx.sample → Fx.small p = true → Fx.big p = true");
    e.amend("Fx.bounded", "offered_as = \"zone-inclusion\"");
    refused(&e.check(), "ZoneInclusionSampled");

    // So is a decision evaluated at one concrete pair of lists.
    let q = Pkg::new("leanprover/lean4:v4.0.0");
    q.require("Fx.evaluated", "Fx.includedIn [Fx.Entry.any] [Fx.Entry.any] = true");
    q.amend("Fx.evaluated", "offered_as = \"zone-inclusion\"");
    refused(&q.check(), "ZoneInclusionSampled");

    // A statement quantifying over every placement is no sample.
    let r = Pkg::new("leanprover/lean4:v4.0.0");
    r.require("Fx.symbolic", "∀ (small big : List Fx.Entry), Fx.includedIn small big = true ↔ ∀ (p : Fx.Placement), Fx.small p = true → Fx.big p = true");
    r.amend("Fx.symbolic", "offered_as = \"zone-inclusion\"");
    let out = r.check();
    assert!(!String::from_utf8_lossy(&out.stderr).contains("ZoneInclusionSampled"), "{}", String::from_utf8_lossy(&out.stderr));
}

/// A filter-composition theorem offered as a mediation theorem raises `MediationClaimedFromComposition`.
// spec: assurance.prove.mediation-from-composition@0c73b06a
#[test]
fn a_composition_theorem_offered_as_mediation_is_refused() {
    let p = Pkg::new("leanprover/lean4:v4.0.0");
    p.require("Fx.mediates", "∀ (ls : List Fx.Layer) (r : Nat), Fx.composed ls r = true → ∀ (l : Fx.Layer), l ∈ ls → l r = true");
    p.amend("Fx.mediates", "offered_as = \"mediation\"");
    let err = refused(&p.check(), "MediationClaimedFromComposition");
    assert!(err.contains("Fx.mediates"), "{err}");
}

/// A proof target is bound to one authenticated query path, recorded beside it as a module path and a test, before the claim names it.
// spec: assurance.prove.target-binding@663d9541
#[test]
fn a_target_binds_a_module_path_and_a_test_in_the_tree() {
    let bound = "binding   = \"engine/fx/src/lib.rs::narrows; test engine/fx/tests/fx.rs::narrows_holds\"\n";

    // A binding naming no test is no binding.
    let p = Pkg::new("leanprover/lean4:v4.0.0");
    p.write("inventory.toml", &p.read("inventory.toml").replace(bound, "binding   = \"engine/fx/src/lib.rs::narrows\"\n"));
    let err = refused(&p.check(), "ProofTargetUnbound");
    assert!(err.contains("Fx.narrows") && err.contains("no test"), "{err}");

    // A module path naming no code in the tree is no binding.
    let q = Pkg::new("leanprover/lean4:v4.0.0");
    q.write(
        "inventory.toml",
        &q.read("inventory.toml").replace(bound, "binding   = \"engine/fx/src/lib.rs::gone; test engine/fx/tests/fx.rs::narrows_holds\"\n"),
    );
    let err = refused(&q.check(), "ProofTargetUnbound");
    assert!(err.contains("engine/fx/src/lib.rs::gone"), "{err}");

    // Nor is a test naming no test function.
    let r = Pkg::new("leanprover/lean4:v4.0.0");
    r.write(
        "inventory.toml",
        &r.read("inventory.toml").replace(bound, "binding   = \"engine/fx/src/lib.rs::narrows; test engine/fx/tests/fx.rs::absent\"\n"),
    );
    let err = refused(&r.check(), "ProofTargetUnbound");
    assert!(err.contains("engine/fx/tests/fx.rs::absent"), "{err}");

    // Every target of the policy package binds a module path and a test that resolve.
    let inv = repo_inventory("formal");
    let targets = inv["claim"]["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 3);
    for t in targets {
        let binding = inv["constant"][t.as_str().unwrap()]["binding"].as_str().unwrap();
        let (decision, test) = binding.split_once("; test ").expect("a module path and a test");
        let decision = decision.split_whitespace().next().unwrap();
        for (path, needle) in [(decision, ""), (test.trim(), "fn ")] {
            let (file, item) = path.split_once("::").unwrap();
            let text = std::fs::read_to_string(repo().join(file)).unwrap();
            let item = item.rsplit("::").next().unwrap();
            assert!(text.contains(&format!("{needle}{item}")), "{t}: {path}");
        }
    }
}

/// A claim that the enforcement engine, or any object wider than the named decisions, is verified raises `ClaimBeyondNamedDecisions`.
// spec: assurance.scope-claim.beyond-named-decisions@447a520d
#[test]
fn a_claim_verifying_more_than_its_decisions_is_refused() {
    for wider in ["the enforcement engine", "engine/fx", "engine/fx/src/lib.rs"] {
        let p = Pkg::new("leanprover/lean4:v4.0.0");
        p.claim(&format!("verified = [\"engine/fx/src/lib.rs::narrows\", \"{wider}\"]"));
        let err = refused(&p.check(), "ClaimBeyondNamedDecisions");
        assert!(err.contains(wider), "{err}");
    }
    // The named decision itself is within the claim.
    let q = Pkg::new("leanprover/lean4:v4.0.0");
    q.claim("verified = [\"engine/fx/src/lib.rs::narrows\"]");
    assert!(!String::from_utf8_lossy(&q.check().stderr).contains("ClaimBeyondNamedDecisions"));
}

const CHAIN: &str = "translated = \"engine/fx/src/lib.rs\"\n\
                     translation = { lowering = \"rustc 1.85 MIR via charon\", translator = \"aeneas 0.1.0\", \
                     models = \"formal/translation/External.lean\", build = \"release profile, panic = abort\" }";

/// A theorem claimed over translated code without its translation chain raises `TranslationChainUnstated`.
// spec: assurance.scope-claim.unstated-chain@4ced56f5
#[test]
fn a_translated_theorem_without_its_chain_is_refused() {
    let p = Pkg::new("leanprover/lean4:v4.0.0");
    p.amend("Fx.narrows", "translated = \"engine/fx/src/lib.rs\"");
    let err = refused(&p.check(), "TranslationChainUnstated");
    assert!(err.contains("Fx.narrows") && err.contains("lowering"), "{err}");

    let q = Pkg::new("leanprover/lean4:v4.0.0");
    q.amend("Fx.narrows", &CHAIN.replace("build = \"release profile, panic = abort\"", "build = \"\""));
    let err = refused(&q.check(), "TranslationChainUnstated");
    assert!(err.contains("`build`"), "{err}");

    // A translator other than Aeneas at one release string leaves the chain unstated.
    for translator in ["hax 0.2", "aeneas"] {
        let r = Pkg::new("leanprover/lean4:v4.0.0");
        r.amend("Fx.narrows", &CHAIN.replace("aeneas 0.1.0", translator));
        let err = refused(&r.check(), "TranslationChainUnstated");
        assert!(err.contains(translator), "{err}");
    }
}

/// Translation reaching cryptography, a parsing adapter, a database call or concurrency raises `RefinementScopeExceeded`, naming the module.
// spec: assurance.scope-claim.refinement-exceeded@91881233
#[test]
fn translation_reaching_past_a_pure_decision_is_refused() {
    for (source, kind) in [
        ("use ed25519_dalek::VerifyingKey;\n", "cryptography"),
        ("use serde_json::Value;\n", "a parsing adapter"),
        ("fn q() { let _ = duckdb::Connection::open_in_memory(); }\n", "a database call"),
        ("fn spawn() { std::thread::spawn(|| ()); }\n", "concurrency"),
        ("async fn later() {}\n", "concurrency"),
    ] {
        let p = Pkg::new("leanprover/lean4:v4.0.0");
        p.amend("Fx.narrows", CHAIN);
        let lib = std::fs::read_to_string(p.repo.join("engine/fx/src/lib.rs")).unwrap();
        p.write_tree("engine/fx/src/lib.rs", &format!("{source}{lib}"));
        let err = refused(&p.check(), "RefinementScopeExceeded");
        assert!(err.contains("engine/fx/src/lib.rs") && err.contains(kind), "{source}: {err}");
    }
    // A word that merely contains an excluded root reaches nothing.
    let q = Pkg::new("leanprover/lean4:v4.0.0");
    q.amend("Fx.narrows", CHAIN);
    let lib = std::fs::read_to_string(q.repo.join("engine/fx/src/lib.rs")).unwrap();
    q.write_tree("engine/fx/src/lib.rs", &format!("/// A string, a tomlish ring_buffer.\n{lib}"));
    assert!(!String::from_utf8_lossy(&q.check().stderr).contains("RefinementScopeExceeded"));
}

/// Refinement covers the decision functions behind the proof targets, whose inputs are values and whose outputs are decisions.
// spec: assurance.scope-claim.refinement-scope@99440669
#[test]
fn refinement_covers_only_the_decisions_behind_the_targets() {
    // A translated constant that is no proof target reaches past the scope.
    let p = Pkg::new("leanprover/lean4:v4.0.0");
    p.amend("Fx.extensional", CHAIN);
    let err = refused(&p.check(), "RefinementScopeExceeded");
    assert!(err.contains("Fx.extensional"), "{err}");

    // So does a target whose translated module is not the one its binding names.
    let q = Pkg::new("leanprover/lean4:v4.0.0");
    q.write_tree("engine/fx/src/other.rs", "pub fn other() {}\n");
    q.amend("Fx.narrows", &CHAIN.replace("translated = \"engine/fx/src/lib.rs\"", "translated = \"engine/fx/src/other.rs\""));
    let err = refused(&q.check(), "RefinementScopeExceeded");
    assert!(err.contains("engine/fx/src/other.rs"), "{err}");

    // The target's own pure decision module is within it.
    let r = Pkg::new("leanprover/lean4:v4.0.0");
    r.amend("Fx.narrows", CHAIN);
    assert!(!String::from_utf8_lossy(&r.check().stderr).contains("RefinementScopeExceeded"));
}

/// The assurance claim reads: these named decisions satisfy these named Lean specifications under these stated translation and runtime assumptions; each list resolves to code paths, constants and components in the tree.
// spec: assurance.scope-claim.claim-sentence@50e8ee1d
#[test]
fn check_states_the_claim_sentence() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    let out = passed(&p.check());
    let sentence = out.lines().find_map(|l| l.strip_prefix("claim: ")).expect("the claim sentence");
    assert_eq!(
        sentence,
        "these named decisions (engine/fx/src/lib.rs::narrows) satisfy these named Lean specifications (Fx.narrows) \
         under these stated translation and runtime assumptions (trusted: delegation-library, translator; translation: none)"
    );
    let claim = &p.report()["claim"];
    assert_eq!(claim["decisions"], serde_json::json!(["engine/fx/src/lib.rs::narrows"]));
    assert_eq!(claim["specifications"], serde_json::json!(["Fx.narrows"]));
    assert_eq!(claim["trusted"], serde_json::json!(["delegation-library", "translator"]));
    // The specification resolves to a constant of the elaborated environment.
    let row = p.report()["constants"].as_array().unwrap().iter().find(|c| c["name"] == "Fx.narrows").unwrap().clone();
    assert_eq!(row["present"], true);
}

/// A statement about translated code inherits four links: the compiler's lowering, the translator (Aeneas, named by one release string), hand-written models of external definitions, and the production build configuration.
// spec: assurance.scope-claim.translation-chain@a7dca82e
#[test]
fn a_translated_target_carries_its_four_links_into_the_claim() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    p.amend("Fx.narrows", CHAIN);
    let out = passed(&p.check());
    let chain = "Fx.narrows: lowering: rustc 1.85 MIR via charon; translator: aeneas 0.1.0; \
                 models: formal/translation/External.lean; build: release profile, panic = abort";
    assert!(out.contains(chain), "{out}");
    assert_eq!(p.report()["claim"]["translation"], serde_json::json!([chain]));
}

/// The claim names the delegation library, its cryptography, its parser and the translator as trusted dependencies, and proves none of them.
// spec: assurance.scope-claim.trusted-dependencies@22d8b80a
#[test]
fn the_claim_trusts_the_delegation_library_and_the_translator() {
    let inv = repo_inventory("formal");
    let trusted: Vec<&str> = inv["claim"]["trusted"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(
        trusted,
        ["delegation-library", "delegation-library-cryptography", "delegation-library-parser", "translator"]
    );
    // No required constant is bound into the delegation library: the model defines no
    // credential bytes, signatures or parser.
    for (name, row) in inv["constant"].as_table().unwrap() {
        let binding = row.get("binding").and_then(|b| b.as_str()).unwrap_or_default();
        assert!(!binding.contains("biscuit"), "{name} binds into the delegation library: {binding}");
    }
    for file in lean_sources(&repo().join("formal/Contextful")) {
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("biscuit") && !text.contains("Biscuit"), "{}", file.display());
    }
}

// ---------------------------------------------------------------- the packages in the tree

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn repo_inventory(package: &str) -> toml::Table {
    toml::from_str(&std::fs::read_to_string(repo().join(package).join("inventory.toml")).unwrap()).unwrap()
}

/// Every `.lean` file under `dir`, artifacts excluded.
fn lean_sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() && entry.file_name() != ".lake" {
            out.extend(lean_sources(&path));
        } else if path.extension().is_some_and(|e| e == "lean") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Copies a package from the tree without its artifacts or nested packages.
fn copy_package(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if entry.file_name() == ".lake" {
            continue;
        }
        if path.is_dir() {
            if path.join("lakefile.toml").is_file() {
                continue;
            }
            copy_package(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

/// A copy of one of the tree's Lean packages at `<tmp>/repo/<package>`, with the code its
/// targets bind to.
fn tree_package(package: &str, home_of: &Pkg) -> PathBuf {
    let repo_copy = home_of.repo.clone();
    let root = repo_copy.join(package);
    let _ = std::fs::remove_dir_all(&root);
    copy_package(&repo().join(package), &root);
    let inv = repo_inventory(package);
    for row in inv.get("constant").and_then(|c| c.as_table()).into_iter().flat_map(|t| t.values()) {
        let Some(binding) = row.get("binding").and_then(|b| b.as_str()) else { continue };
        for part in binding.split(';') {
            let path = part.trim().trim_start_matches("test ").split_whitespace().next().unwrap_or_default();
            if let Some((file, _)) = path.split_once("::") {
                let dest = repo_copy.join(file);
                std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                std::fs::copy(repo().join(file), dest).unwrap();
            }
        }
    }
    root
}

fn words(s: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for token in s.split(|c: char| !c.is_alphanumeric()) {
        let mut word = String::new();
        let mut prev_lower = false;
        for ch in token.chars() {
            if ch.is_uppercase() && prev_lower {
                out.insert(std::mem::take(&mut word));
            }
            prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
            word.extend(ch.to_lowercase());
        }
        out.insert(word);
    }
    out.into_iter().filter(|w| w.len() > 2).map(|w| w.trim_end_matches('s').to_string()).collect()
}

/// The policy model is a Lean package rooted at `formal/`: `lakefile.toml` declares one `lean_lib` target and no `require` stanza, and `lean-toolchain` names one exact release string.
// spec: assurance.model.package@fb878811
#[test]
fn the_policy_package_has_one_library_no_dependency_and_an_exact_toolchain() {
    let formal = repo().join("formal");
    let lakefile: toml::Table = toml::from_str(&std::fs::read_to_string(formal.join("lakefile.toml")).unwrap()).unwrap();
    assert_eq!(lakefile["lean_lib"].as_array().unwrap().len(), 1);
    assert!(lakefile.get("lean_exe").is_none() && lakefile.get("require").is_none(), "{lakefile}");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(formal.join("lake-manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["packages"], serde_json::json!([]));
    let pin = std::fs::read_to_string(formal.join("lean-toolchain")).unwrap();
    let version = pin.trim().strip_prefix("leanprover/lean4:v").expect("a lean4 release");
    let parts: Vec<&str> = version.split('.').collect();
    assert_eq!(parts.len(), 3, "{pin}");
    assert!(parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())), "one exact release: {pin}");
}

/// Four modules carry the package — the layer algebra, placement with the floor, the authority mapping and connector host containment — and the library root re-exports all four.
// spec: assurance.model.module-layout@6a3957c1
#[test]
fn the_library_root_re_exports_four_modules() {
    let formal = repo().join("formal");
    let root = std::fs::read_to_string(formal.join("Contextful.lean")).unwrap();
    let imports: Vec<&str> = root.lines().filter_map(|l| l.strip_prefix("import ")).map(str::trim).collect();
    assert_eq!(imports, ["Contextful.Layer", "Contextful.Placement", "Contextful.Authority", "Contextful.Allowlist"]);
    let modules: Vec<PathBuf> = lean_sources(&formal.join("Contextful"));
    assert_eq!(modules.len(), 4, "{modules:?}");
    for (module, carries) in [
        ("Layer", "def composed"),
        ("Placement", "def floor"),
        ("Authority", "def attenuate"),
        ("Allowlist", "def includedIn"),
    ] {
        let text = std::fs::read_to_string(formal.join(format!("Contextful/{module}.lean"))).unwrap();
        assert!(text.contains(carries), "{module} carries `{carries}`");
    }
}

/// Each definition is written from the specification text and derived from no engine source.
// spec: assurance.model.from-the-spec@acaa04a2
#[test]
fn the_models_import_no_engine_source() {
    let formal = repo().join("formal");
    let mut sources = lean_sources(&formal.join("Contextful"));
    sources.push(formal.join("Contextful.lean"));
    sources.extend(lean_sources(&formal.join("protocol/Protocol")));
    assert!(sources.len() >= 7);
    for file in sources {
        let text = std::fs::read_to_string(&file).unwrap();
        for line in text.lines().filter_map(|l| l.strip_prefix("import ")) {
            let ok = ["Contextful.", "Protocol."].iter().any(|p| line.starts_with(p)) || line.starts_with("Std.");
            assert!(ok, "{}: imports `{line}`", file.display());
        }
        for engine in ["crates/", ".rs:", ".rs::", "contextful_core", "include_str", "#include"] {
            assert!(!text.contains(engine), "{} reaches engine source through `{engine}`", file.display());
        }
    }
}

/// Beside each theorem, its negative space states what it leaves open: that the required members were in the list, that a member removes rather than tests, and that execution reaches the fold before an effect.
// spec: assurance.prove.negative-space@2fe9879b
#[test]
fn every_composition_theorem_states_what_it_leaves_open() {
    let mut composition = 0;
    for package in ["formal", "formal/protocol"] {
        let inv = repo_inventory(package);
        for (name, row) in inv["constant"].as_table().unwrap() {
            let negative = row["negative"].as_str().unwrap().trim();
            assert!(negative.starts_with("Leaves open:"), "{name}: {negative}");
            if row["statement"].as_str().unwrap().contains("composed") {
                composition += 1;
                for open in [
                    "the required members were present in the list",
                    "a member performs removal rather than a test",
                    "an execution reaches the fold ahead of an effect",
                ] {
                    assert!(negative.contains(open), "{name} leaves `{open}` unstated");
                }
            }
        }
    }
    assert_eq!(composition, 3, "composed_sound, composed_narrows and zone_layer_sound");
}

/// A constant's name states the object it ranges over, not the property a reader hopes for.
// spec: assurance.prove.names-carry-reach@3dab3c0b
#[test]
fn every_constant_name_carries_the_object_it_ranges_over() {
    let mut rows = 0;
    for package in ["formal", "formal/protocol"] {
        let inv = repo_inventory(package);
        for (name, row) in inv["constant"].as_table().unwrap() {
            rows += 1;
            let last = name.rsplit('.').next().unwrap();
            let named = words(last);
            for hoped in ["secure", "verified", "correct", "mediate", "complete"] {
                assert!(!named.iter().any(|w| w.starts_with(hoped)), "{name} names `{hoped}`, a property, not an object");
            }
            let statement = words(row["statement"].as_str().unwrap());
            assert!(named.intersection(&statement).next().is_some(), "{name} names nothing its statement ranges over");
        }
    }
    assert!(rows >= 18, "{rows}");
    // A name carrying only a hoped-for property fails.
    assert!(words("engine_is_secure").intersection(&words("∀ (r : RowId), composed [] r = true")).next().is_none());
}

/// `formal/inventory.toml` holds one row per required constant — name, module, expected statement text, admitted assumptions, binding, negative space — edited apart from the declarations satisfying it.
// spec: assurance.audit-assumptions.inventory@f36ff6f1
#[test]
fn every_inventory_row_carries_its_six_fields() {
    let inv = repo_inventory("formal");
    let targets: Vec<&str> = inv["claim"]["targets"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
    let rows = inv["constant"].as_table().unwrap();
    assert!(rows.len() >= 12, "{}", rows.len());
    for (name, row) in rows {
        let module = row["module"].as_str().unwrap();
        assert!(module.starts_with("Contextful."), "{name}: {module}");
        assert!(!row["statement"].as_str().unwrap().trim().is_empty(), "{name}");
        let assumptions: Vec<&str> = row["assumptions"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
        assert!(assumptions.iter().all(|a| ["propext", "Quot.sound"].contains(a)), "{name}: {assumptions:?}");
        assert!(!row["negative"].as_str().unwrap().trim().is_empty(), "{name}");
        if targets.contains(&name.as_str()) {
            assert!(!row["binding"].as_str().unwrap().trim().is_empty(), "{name}");
        }
        // The declaration lives in its module's source, apart from the inventory.
        let last = name.rsplit('.').next().unwrap();
        let file = repo().join("formal").join(module.replace('.', "/")).with_extension("lean");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(&format!("theorem {last}")), "{name} in {}", file.display());
    }
    // No Lean source reads the inventory: the declarations do not generate it, nor it them.
    for file in lean_sources(&repo().join("formal")) {
        assert!(!std::fs::read_to_string(&file).unwrap().contains("inventory"), "{}", file.display());
    }
}

/// The protocol model is a second Lean 4 package rooted at `formal/protocol/`, beside the policy package, pinned to the same `lean-toolchain` string, with one `lean_lib` target, one `lean_exe` target and no `require` stanza.
// spec: assurance.model.protocol-package@662befd3
#[test]
fn the_protocol_package_sits_beside_the_policy_package() {
    let formal = repo().join("formal");
    let protocol = formal.join("protocol");
    let lakefile: toml::Table = toml::from_str(&std::fs::read_to_string(protocol.join("lakefile.toml")).unwrap()).unwrap();
    assert_eq!(lakefile["lean_lib"].as_array().unwrap().len(), 1);
    assert_eq!(lakefile["lean_exe"].as_array().unwrap().len(), 1);
    assert!(lakefile.get("require").is_none(), "{lakefile}");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(protocol.join("lake-manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["packages"], serde_json::json!([]));
    assert_eq!(
        std::fs::read_to_string(protocol.join("lean-toolchain")).unwrap().trim(),
        std::fs::read_to_string(formal.join("lean-toolchain")).unwrap().trim()
    );
}

/// {{store.lease.stale-fence}} and {{store.lease.pointer-fence}} pin to the protocol model's invariant theorem `no_commit_below_granted`.
// spec: assurance.model.protocol-pins@aaeebf1d
#[test]
fn the_store_fence_clauses_pin_to_the_protocol_invariant() {
    let text = std::fs::read_to_string(repo().join("formal/protocol/Protocol/Invariants.lean")).unwrap();
    let at = text.find("theorem no_commit_below_granted").expect("the invariant theorem");
    let above: Vec<&str> = text[..at].lines().rev().take_while(|l| l.starts_with("--")).collect();
    for clause in ["store.lease.stale-fence@", "store.lease.pointer-fence@"] {
        assert!(above.iter().any(|l| l.starts_with(&format!("-- spec: {clause}"))), "{clause} above the theorem: {above:?}");
    }
    assert!(repo_inventory("formal/protocol")["constant"].as_table().unwrap().contains_key("Protocol.no_commit_below_granted"));
}

/// A cold elaboration of the package completes within 60 s and writes at most 2048 KiB of artifacts.
// spec: assurance.model.build-cost@dfe87d45
#[test]
fn a_cold_build_fits_its_time_and_artifact_budget() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    let root = tree_package("formal", &p);
    if std::fs::read_to_string(root.join("lean-toolchain")).unwrap().trim() != t {
        eprintln!("skipped: the pinned toolchain is not the installed default");
        return;
    }
    assert!(!root.join(".lake").exists());
    let started = std::time::Instant::now();
    let out = Command::new("lake").arg("build").current_dir(&root).output().unwrap();
    let elapsed = started.elapsed();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(elapsed < std::time::Duration::from_secs(60), "{elapsed:?}");
    fn bytes(dir: &Path) -> u64 {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| if e.path().is_dir() { bytes(&e.path()) } else { e.metadata().unwrap().len() })
            .sum()
    }
    let written = bytes(&root.join(".lake"));
    assert!(written > 0 && written <= 2048 * 1024, "{written} bytes");
}

/// `lake build` at the package root elaborates every declaration and writes the environment the audit reads.
// spec: assurance.model.build-command@1951abdf
#[test]
fn lake_build_writes_the_environment_the_audit_reads() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    let root = tree_package("formal", &p);
    if std::fs::read_to_string(root.join("lean-toolchain")).unwrap().trim() != t {
        eprintln!("skipped: the pinned toolchain is not the installed default");
        return;
    }
    let out = p.command().args(["formal", "check", "--root"]).arg(&root).output().unwrap();
    passed(&out);
    for module in ["Layer", "Placement", "Authority", "Allowlist"] {
        assert!(root.join(format!(".lake/build/lib/lean/Contextful/{module}.olean")).is_file(), "{module}");
    }
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".lake/contextful-report.json")).unwrap()).unwrap();
    let rows = report["constants"].as_array().unwrap();
    assert_eq!(rows.len(), repo_inventory("formal")["constant"].as_table().unwrap().len());
    assert!(rows.iter().all(|r| r["verdict"] == "ok" && r["present"] == true), "{report}");
}

/// A category the manifest admits with no case in the placement inductive raises `UnmodelledConstructor` at elaboration, naming the category.
// spec: assurance.model.unmodelled-constructor@a296eadc
#[test]
fn a_manifest_category_without_a_case_is_refused() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    let root = tree_package("formal", &p);
    if std::fs::read_to_string(root.join("lean-toolchain")).unwrap().trim() != t {
        eprintln!("skipped: the pinned toolchain is not the installed default");
        return;
    }
    let placement = root.join("Contextful/Placement.lean");
    let text = std::fs::read_to_string(&placement).unwrap();
    let categories = "[\"local\", \"on-prem\", \"private-cloud\", \"public-cloud\"]";
    assert!(text.contains(categories));
    std::fs::write(&placement, text.replace(categories, "[\"local\", \"on-prem\", \"private-cloud\", \"public-cloud\", \"edge\"]"))
        .unwrap();
    let out = p.command().args(["formal", "check", "--root"]).arg(&root).output().unwrap();
    let err = refused(&out, "UnmodelledConstructor");
    assert!(err.contains("`edge`"), "{err}");
}

const PROBE: &str = r#"import Lean
import Contextful
open Lean Meta

def outOfModel : List Name := [`IO, `EIO, `BaseIO, `ST, `EST, `ByteArray, `Task, `System]

#eval show MetaM Unit from do
  let env ← getEnv
  let mods := env.header.moduleNames
  let mut total := 0
  for (n, ci) in env.constants.map₁.toList do
    let some idx := env.getModuleIdxFor? n | continue
    unless (`Contextful).isPrefixOf mods[idx.toNat]! do continue
    total := total + 1
    if ci matches .opaqueInfo _ then IO.println s!"opaque {n}"
    if ci.isUnsafe then IO.println s!"unsafe {n}"
    if isNoncomputable env n then IO.println s!"noncomputable {n}"
    if ci matches .axiomInfo _ then IO.println s!"axiom {n}"
    let used := ci.type.getUsedConstants ++ (ci.value?.map (·.getUsedConstants) |>.getD #[])
    for u in used do
      if outOfModel.contains u.getRoot then IO.println s!"reaches {n} {u}"
    if let .inductInfo ii := ci then
      let ctors := ii.ctors.filterMap env.find?
      let enum := !ii.isRec && ii.numIndices == 0 && ii.numParams == 0 &&
        ctors.all (fun c => match c with | .ctorInfo c => c.numFields == 0 | _ => false)
      if enum then
        let inst ← synthInstance? (mkApp (mkConst ``DecidableEq [1]) (mkConst n))
        IO.println s!"enumeration {n} {if inst.isSome then "derived" else "underived"}"
  IO.println s!"declarations {total}"
"#;

/// Builds a copy of the policy package, applies `edit` to its Placement module first, and
/// returns what the probe prints over every declaration of the package.
fn probe(edit: impl FnOnce(&str) -> String) -> Option<String> {
    let t = toolchain()?;
    let p = Pkg::new(&t);
    let root = tree_package("formal", &p);
    if std::fs::read_to_string(root.join("lean-toolchain")).unwrap().trim() != t {
        eprintln!("skipped: the pinned toolchain is not the installed default");
        return None;
    }
    let placement = root.join("Contextful/Placement.lean");
    std::fs::write(&placement, edit(&std::fs::read_to_string(&placement).unwrap())).unwrap();
    let build = Command::new("lake").arg("build").current_dir(&root).output().unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stdout));
    std::fs::write(root.join(".lake/Probe.lean"), PROBE).unwrap();
    let out = Command::new("lake").args(["env", "lean", ".lake/Probe.lean"]).current_dir(&root).output().unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Every definition is total and computable: none is marked `partial`, `noncomputable` or `opaque`, and decidable equality on each finite enumeration is derived.
// spec: assurance.model.total-definitions@2dac9c34
#[test]
fn every_definition_is_total_and_computable() {
    let _ = lean_or_skip!();
    let Some(clean) = probe(|s| s.to_string()) else { return };
    let flagged: Vec<&str> = clean
        .lines()
        .filter(|l| ["opaque ", "unsafe ", "noncomputable ", "axiom "].iter().any(|k| l.starts_with(k)) || l.ends_with("underived"))
        .collect();
    assert!(flagged.is_empty(), "{flagged:?}");
    assert!(clean.contains("enumeration Category derived") && clean.contains("enumeration Authority.Action derived"), "{clean}");
    let declarations: usize = clean.lines().find_map(|l| l.strip_prefix("declarations ")).unwrap().parse().unwrap();
    assert!(declarations > 100, "{declarations}");

    // The probe sees each marking it refuses.
    let marked = probe(|s| {
        format!(
            "{s}\n\npartial def spin (n : Nat) : Nat := spin n\n\nnoncomputable def chosen : Nat := Classical.choice ⟨0⟩\n\n\
             opaque hidden : Nat\n\ninductive Shade where\n  | dark\n  | light\n"
        )
    })
    .unwrap();
    for line in ["opaque spin", "noncomputable chosen", "opaque hidden", "enumeration Shade underived"] {
        assert!(marked.lines().any(|l| l == line), "{line} in {marked}");
    }
}

/// The policy package defines no credential bytes, signature verification, SQL semantics, journal write, process state or attacker observation, and no theorem reaches one.
// spec: assurance.model.out-of-model@aee1c013
#[test]
fn the_policy_package_reaches_no_process_state_or_bytes() {
    let _ = lean_or_skip!();
    let Some(clean) = probe(|s| s.to_string()) else { return };
    let reaches: Vec<&str> = clean.lines().filter(|l| l.starts_with("reaches ")).collect();
    assert!(reaches.is_empty(), "{reaches:?}");
    // A declaration carrying bytes or process state is seen.
    let marked = probe(|s| format!("{s}\n\ndef credential : ByteArray := ByteArray.empty\n\ndef journal : IO Unit := pure ()\n")).unwrap();
    assert!(marked.lines().any(|l| l.starts_with("reaches credential ByteArray")), "{marked}");
    assert!(marked.lines().any(|l| l.starts_with("reaches journal IO")), "{marked}");
}

/// Each protocol invariant theorem is a required constant in the protocol package's inventory, audited by {{assurance.audit-assumptions.check-command}} against the same allowlist.
// spec: assurance.model.protocol-theorems@f4541b68
#[test]
fn the_protocol_invariants_pass_the_same_audit() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    let root = tree_package("formal/protocol", &p);
    if std::fs::read_to_string(root.join("lean-toolchain")).unwrap().trim() != t {
        eprintln!("skipped: the pinned toolchain is not the installed default");
        return;
    }
    let out = passed(&p.command().args(["formal", "check", "--root"]).arg(&root).output().unwrap());
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".lake/contextful-report.json")).unwrap()).unwrap();
    assert_eq!(report["allowlist"], serde_json::json!(["propext", "Quot.sound"]));
    for invariant in [
        "Protocol.one_holder_per_fence",
        "Protocol.fences_only_increase",
        "Protocol.no_commit_below_granted",
        "Protocol.release_keeps_lease",
    ] {
        assert!(out.contains(invariant), "{out}");
        let row = report["constants"].as_array().unwrap().iter().find(|r| r["name"] == invariant).unwrap().clone();
        assert_eq!(row["verdict"], "ok", "{row}");
    }
}

/// The gate evaluates every invariant on each state reached by every step sequence over three nodes and four lease generations; a breaking state raises `ProtocolInvariantViolated`, printing the shortest sequence reaching it.
// spec: assurance.model.protocol-check@2673f4f2
#[test]
fn the_bounded_check_covers_three_nodes_and_four_generations() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    let root = tree_package("formal/protocol", &p);
    if std::fs::read_to_string(root.join("lean-toolchain")).unwrap().trim() != t {
        eprintln!("skipped: the pinned toolchain is not the installed default");
        return;
    }
    let ok = Command::new("lake").args(["exe", "protocol", "check"]).current_dir(&root).output().unwrap();
    let out = passed(&ok);
    assert!(out.contains("4 invariants hold") && out.contains("over 3 nodes and 4 lease generations"), "{out}");

    // The variant whose grant leaves the guarded fences untouched breaks an invariant, and
    // the check prints the shortest sequence reaching the break.
    let broken = Command::new("lake").args(["exe", "protocol", "check", "--unfenced"]).current_dir(&root).output().unwrap();
    let err = refused(&broken, "ProtocolInvariantViolated");
    let steps: Vec<&str> = err.lines().filter(|l| l.starts_with("  ")).collect();
    let claimed: usize = err.split("after ").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
    assert!(!steps.is_empty() && steps.len() <= claimed, "{err}");
}

/// An assumption is a declaration the Lean kernel accepts without proof: the set `#print axioms` reports for a constant, which the audit reads through `collectAxioms`.
// spec: assurance.audit-assumptions.assumption@fd02a55d
#[test]
fn a_declared_axiom_is_an_assumption() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    p.declare("axiom trusted : ∀ (n : Nat), n = n + 0\n\ntheorem viaAxiom (n : Nat) : n = n + 0 := trusted n");
    p.require("Fx.viaAxiom", "∀ (n : Nat), n = n + 0");
    let err = refused(&p.check(), "AssumptionOutsideAllowlist");
    assert!(err.contains("Fx.trusted"), "{err}");
    let row = p.report()["constants"].as_array().unwrap().iter().find(|c| c["name"] == "Fx.viaAxiom").unwrap().clone();
    assert_eq!(row["assumptions"], serde_json::json!(["Fx.trusted"]));
    // A theorem proved outright carries no assumption.
    let narrows = p.report()["constants"].as_array().unwrap().iter().find(|c| c["name"] == "Fx.narrows").unwrap().clone();
    assert_eq!(narrows["assumptions"], serde_json::json!([]));
}

/// A change to a required statement lands in the commit carrying the proof it admits.
// spec: assurance.audit-assumptions.inventory-change@165e4f4d
#[test]
fn a_statement_change_lands_with_its_proof() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    p.commit();
    // The inventory change is committed; the proof it admits is not.
    let inv = p.read("inventory.toml").replace("statement = \"∀ (n : Nat), n + 0 = n\"", "statement = \"∀ (n : Nat), 0 + n = n\"");
    p.write("inventory.toml", &inv);
    p.git(&["commit", "-q", "-am", "statement"]);
    let src = p.read("Fx/Basic.lean").replace("theorem narrows (n : Nat) : n + 0 = n := rfl", "theorem narrows (n : Nat) : 0 + n = n := Nat.zero_add n");
    p.write("Fx/Basic.lean", &src);
    passed(&p.check());
    let err = refused(&p.command().args(["formal", "recheck"]).output().unwrap(), "TheoremStatementDrift");
    assert!(err.contains("Fx.narrows"), "{err}");

    // Landing the proof in the same history closes the gap.
    p.git(&["commit", "-q", "-am", "proof"]);
    passed(&p.command().args(["formal", "recheck"]).output().unwrap());
}

/// A recheck completes within 600 s.
// spec: assurance.recheck.wall-time@abb3ca5e
#[test]
fn a_recheck_of_the_policy_package_completes_within_600_s() {
    let t = lean_or_skip!();
    let p = Pkg::new(&t);
    let root = tree_package("formal", &p);
    if std::fs::read_to_string(root.join("lean-toolchain")).unwrap().trim() != t {
        eprintln!("skipped: the pinned toolchain is not the installed default");
        return;
    }
    p.commit();
    let started = std::time::Instant::now();
    let out = passed(&p.command().args(["formal", "recheck", "--root", "formal"]).output().unwrap());
    let elapsed = started.elapsed();
    assert!(out.contains("Authority.attenuate_permits_parent"), "{out}");
    assert!(elapsed < std::time::Duration::from_secs(600), "{elapsed:?}");
}

const ADMISSION: &str = "Classical choice in the case-split lemmas";

/// A package whose row `Fx.em'` draws on `Classical.choice` and lists it among its
/// admitted assumptions, citing `record` when given.
fn admitting(toolchain: &str, record: Option<&str>) -> Pkg {
    let p = Pkg::new(toolchain);
    p.declare("theorem em' (p : Prop) : p ∨ ¬p := Classical.em p");
    let mut inv = p.read("inventory.toml");
    inv.push_str(
        "\n[constant.\"Fx.em'\"]\nmodule    = \"Fx.Basic\"\nstatement = \"∀ (p : Prop), p ∨ ¬p\"\n\
         assumptions    = [\"propext\", \"Quot.sound\", \"Classical.choice\"]\nnegative  = \"Leaves open: the rest.\"\n",
    );
    if let Some(record) = record {
        inv.push_str(&format!("record    = \"{record}\"\n"));
    }
    p.write("inventory.toml", &inv);
    p
}

/// An assumption joins the allowlist through an `A-assurance` section naming it, and each inventory row admitting it cites that section in a `record` field.
// spec: assurance.audit-assumptions.allowlist-admission@1359866a
#[test]
fn an_assumption_beyond_the_allowlist_needs_a_cited_record() {
    let adr = format!("# A-assurance — Assurance decisions\n\n## {ADMISSION}\n\nThe case-split lemmas draw on `Classical.choice`.\n");

    // A row admitting it with no record is outside the allowlist.
    let p = admitting("leanprover/lean4:v4.0.0", None);
    let err = refused(&p.check(), "AssumptionOutsideAllowlist");
    assert!(err.contains("Fx.em'") && err.contains("Classical.choice"), "{err}");

    // So is a row citing a section that does not name it.
    let q = admitting("leanprover/lean4:v4.0.0", Some("A-assurance: Another decision"));
    q.write_tree("spec/adr/A-assurance.md", &format!("{adr}\n## Another decision\n\nNothing admitted.\n"));
    refused(&q.check(), "AssumptionOutsideAllowlist");

    // A cited section naming it admits it for that row alone.
    let t = lean_or_skip!();
    let r = admitting(&t, Some(&format!("A-assurance: {ADMISSION}")));
    r.write_tree("spec/adr/A-assurance.md", &adr);
    passed(&r.check());
    let rows = r.report()["constants"].as_array().unwrap().clone();
    let em = rows.iter().find(|c| c["name"] == "Fx.em'").unwrap();
    assert_eq!(em["admitted"], serde_json::json!(["propext", "Quot.sound", "Classical.choice"]));
    let narrows = rows.iter().find(|c| c["name"] == "Fx.narrows").unwrap();
    assert_eq!(narrows["admitted"], serde_json::json!(["propext", "Quot.sound"]));
    assert_eq!(r.report()["allowlist"], serde_json::json!(["propext", "Quot.sound"]));
}

/// `floor_no_downgrade`'s negative space names a non-empty evidence list, which {{authority.place.empty-evidence}} holds at the engine.
// spec: assurance.prove.evidence-nonempty@28e0555a
#[test]
fn the_floor_theorem_leaves_non_emptiness_to_the_engine() {
    let inv = repo_inventory("formal");
    let negative = inv["constant"]["floor_no_downgrade"]["negative"].as_str().unwrap();
    assert!(negative.contains("non-empty") && negative.contains("refuses a row naming no evidence table"), "{negative}");
    let place = std::fs::read_to_string(repo().join("crates/contextful-core/src/place.rs")).unwrap();
    assert!(place.contains("EvidenceListEmpty"), "the engine refuses an empty evidence list");
}
