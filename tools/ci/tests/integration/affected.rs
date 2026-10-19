//! `assurance.gate.affected-*`: `contextful-ci stages --parts --base <rev> --affected` prints
//! the required parts the change reaches, in run order.

use crate::{manifest, stderr, Repo};

const LOCK: &str = "spec/spec.lock.json";

fn lock(clauses: &[(&str, &str)]) -> String {
    let items: Vec<String> = clauses.iter().map(|(id, statement)| format!(r#"{{"id":"{id}","kind":"behavior","statement":"{statement}"}}"#)).collect();
    format!(r#"{{"clauses":[{}]}}"#, items.join(","))
}

/// A workspace holding `demo`, a binary package with one listed feature run, a spec lock
/// of three clauses, a tag pinning one to `demo`'s test, a pins entry pinning one to the
/// binary's test, and a Lean tag pinning the third.
fn fixture() -> (Repo, String) {
    let r = Repo::init();
    r.write(
        "crates/contextful-cli/Cargo.toml",
        &format!("{}\n[features]\nextra = []\n\n[package.metadata.contextful]\nfeature-runs = [\"extra\"]\n", manifest("contextful-cli", "")),
    );
    r.write("crates/contextful-cli/src/lib.rs", "");
    r.write("crates/contextful-cli/tests/integration/main.rs", "#[test]\nfn builds() {}\n");
    r.write(
        "crates/demo/tests/integration/double.rs",
        "// spec: assurance.gate.tagged@00000000\n#[test]\nfn doubles() {\n    assert_eq!(demo::double(2), 4);\n}\n",
    );
    r.write("spec/pins.toml", "[pin]\n\"assurance.gate.listed\" = { test = \"contextful_cli::builds\" }\n");
    r.write("formal/Demo.lean", "-- spec: assurance.gate.proved@00000000\ntheorem t : True := trivial\n");
    r.write("spec/81-engineering.md", "# gate\n");
    r.write(LOCK, &lock(&[("assurance.gate.tagged", "A."), ("assurance.gate.listed", "B."), ("assurance.gate.proved", "C.")]));
    r.lock();
    r.commit("fixture");
    let base = r.head();
    (r, base)
}

fn listed(r: &Repo, args: &[&str]) -> Vec<String> {
    let o = r.run_ci(args);
    assert!(o.status.success(), "{}", stderr(&o));
    String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect()
}

fn affected(r: &Repo, base: &str) -> Vec<String> {
    listed(r, &["stages", "--parts", "--base", base, "--affected"])
}

fn required(r: &Repo, base: &str) -> Vec<String> {
    listed(r, &["stages", "--parts", "--base", base])
}

/// The listing is a run-order subsequence of the required set holding pins, toolchain and schema.
fn assert_subset(r: &Repo, base: &str, got: &[String]) {
    let all = required(r, base);
    let mut at = 0;
    for part in got {
        let found = all[at..].iter().position(|p| p == part).unwrap_or_else(|| panic!("`{part}` is out of the required set or out of order: {got:?} of {all:?}"));
        at += found + 1;
    }
    for part in ["pins", "toolchain", "schema"] {
        assert!(got.iter().any(|p| p == part), "`{part}` missing: {got:?}");
    }
}

/// A change under `apps/` or `packages/` reaches the TypeScript surfaces part beside pins, toolchain and schema.
// spec: assurance.gate.affected-surfaces@a56e7b8e
#[test]
fn a_typescript_change_reaches_the_surfaces_part_alone() {
    let (r, base) = fixture();
    r.write("apps/web/src/index.ts", "export const x = 1;\n");
    r.write("packages/kit/src/index.ts", "export const y = 2;\n");
    r.commit("typescript only");
    let got = affected(&r, &base);
    assert_subset(&r, &base, &got);
    assert_eq!(got, ["pins", "toolchain", "schema", "surfaces"]);
}

/// A change under `spec/` or `references/` reaches test-first validation and, for each clause whose statement or `spec/pins.toml` entry changed, the parts running its pinned test or theorem.
// spec: assurance.gate.affected-spec@1822718a
#[test]
fn a_spec_change_reaches_validation_and_the_parts_running_each_changed_clause_pin() {
    let (r, base) = fixture();
    r.write("spec/guide/gate.md", "# The gate\n");
    r.write("references/gate.md", "# Gate practice\n");
    r.commit("prose only");
    assert_eq!(affected(&r, &base), ["pins", "toolchain", "schema", "test-first.validate"]);

    // A reworded clause pinned by a tag in `demo`'s test reaches the parts running `demo`.
    r.write(LOCK, &lock(&[("assurance.gate.tagged", "A, reworded."), ("assurance.gate.listed", "B."), ("assurance.gate.proved", "C.")]));
    r.commit("reword the tagged clause");
    let got = affected(&r, &base);
    assert_subset(&r, &base, &got);
    assert_eq!(got, ["pins", "toolchain", "schema", "test-first.validate", "workspace.compile", "workspace.foundation"]);

    // A clause pinned in `spec/pins.toml` to the binary's test reaches every binary part.
    r.write(LOCK, &lock(&[("assurance.gate.tagged", "A."), ("assurance.gate.listed", "B, reworded."), ("assurance.gate.proved", "C.")]));
    r.commit("reword the listed clause");
    let got = affected(&r, &base);
    assert_subset(&r, &base, &got);
    for part in ["workspace.compile", "workspace.cli", "workspace.cli-formal", "features.binary-extra", "features.formal-extra"] {
        assert!(got.iter().any(|p| p == part), "`{part}` missing: {got:?}");
    }
    assert!(!got.iter().any(|p| p == "workspace.foundation" || p == "surfaces" || p == "formal"), "{got:?}");

    // A clause a Lean theorem pins reaches the formal parts.
    r.write(LOCK, &lock(&[("assurance.gate.tagged", "A."), ("assurance.gate.listed", "B."), ("assurance.gate.proved", "C, reworded.")]));
    r.commit("reword the proved clause");
    let got = affected(&r, &base);
    assert_subset(&r, &base, &got);
    assert!(got.iter().any(|p| p == "formal") && got.iter().any(|p| p == "workspace.cli-formal"), "{got:?}");
    assert!(!got.iter().any(|p| p == "workspace.foundation" || p == "workspace.cli"), "{got:?}");

    // A changed `spec/pins.toml` entry reaches the parts running the test it now names.
    r.write(LOCK, &lock(&[("assurance.gate.tagged", "A."), ("assurance.gate.listed", "B."), ("assurance.gate.proved", "C.")]));
    r.write("spec/pins.toml", "[pin]\n\"assurance.gate.listed\" = { test = \"crates/demo/tests/integration/double.rs::doubles\" }\n");
    r.commit("repin the listed clause");
    let got = affected(&r, &base);
    for part in ["workspace.foundation", "workspace.cli"] {
        assert!(got.iter().any(|p| p == part), "`{part}` missing, the old and new pins both reach: {got:?}");
    }
}

/// A change under `formal/` other than its toolchain file reaches the formal stage and every CLI differential part.
// spec: assurance.gate.affected-formal@dc6ea087
#[test]
fn a_formal_change_reaches_the_formal_and_differential_parts() {
    let (r, base) = fixture();
    r.write("formal/Demo.lean", "-- spec: assurance.gate.proved@00000000\ntheorem t : True := by trivial\n");
    r.commit("reprove");
    let got = affected(&r, &base);
    assert_subset(&r, &base, &got);
    assert_eq!(got, ["pins", "toolchain", "schema", "workspace.cli-formal", "features.formal-none", "features.formal-all", "features.formal-extra", "formal"]);
}

/// A change to `Cargo.toml`, `Cargo.lock`, a toolchain file, anything under `crates/`, `tools/` or `.github/`, or any path no other affected clause maps reaches every required part.
// spec: assurance.gate.affected-every@35219866
#[test]
fn a_build_input_or_an_unmapped_path_reaches_every_required_part() {
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "formal/lean-toolchain",
        "crates/demo/src/lib.rs",
        "crates/demo/tests/integration/double.rs",
        "tools/ci/README.md",
        ".github/rulesets/gate.json",
        "README.md",
        "Dockerfile",
    ] {
        let (r, base) = fixture();
        let before = std::fs::read_to_string(r.root.join(path)).unwrap_or_default();
        r.write(path, &format!("{before}\n"));
        r.commit(path);
        assert_eq!(affected(&r, &base), required(&r, &base), "`{path}` reaches only some parts");
    }
}

/// An unreadable spec lock or pins file, or a changed clause pinned outside every workspace package, reaches every required part.
// spec: assurance.gate.affected-unplaced@2a60cec5
#[test]
fn a_pin_the_listing_cannot_place_reaches_every_required_part() {
    // A spec lock the listing cannot read reaches every part.
    let (r, base) = fixture();
    r.write(LOCK, "{ not json");
    r.commit("unreadable lock");
    assert_eq!(affected(&r, &base), required(&r, &base));

    // A changed clause pinned to a test whose package the workspace does not hold reaches every part.
    let (r, base) = fixture();
    r.write("spec/pins.toml", "[pin]\n\"assurance.gate.listed\" = { test = \"contextful_absent::gone\" }\n");
    r.commit("pin outside the workspace");
    assert_eq!(affected(&r, &base), required(&r, &base));

    // A pins file the listing cannot read reaches every part.
    let (r, base) = fixture();
    r.write("spec/pins.toml", "[pin\n");
    r.commit("unreadable pins");
    assert_eq!(affected(&r, &base), required(&r, &base));
}

/// Without `--base`, `--affected` is refused.
#[test]
fn affected_requires_a_base() {
    let (r, _) = fixture();
    let o = r.run_ci(&["stages", "--parts", "--affected"]);
    assert!(!o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
}

/// `contextful-ci stages --parts --base <rev> --affected` lists, in run order, the required parts the diff from the merge base of `<rev>` and HEAD reaches, always including pins, toolchain and schema.
// spec: assurance.gate.affected-parts@9f96acab
#[test]
fn affected_measures_from_the_merge_base() {
    let (r, base) = fixture();
    r.git(&["checkout", "-q", "-b", "topic"]);
    r.write("apps/web/src/index.ts", "export const x = 1;\n");
    r.commit("typescript only");
    r.git(&["checkout", "-q", "main"]);
    r.write("crates/demo/src/lib.rs", "pub fn double(x: i32) -> i32 {\n    x + x\n}\n");
    let main = r.commit("main moves on");
    r.git(&["checkout", "-q", "topic"]);
    assert_ne!(main, base);
    // The base gained a source change after the branch; the listing measures the branch alone.
    assert_eq!(affected(&r, &main), ["pins", "toolchain", "schema", "surfaces"]);

    // An empty diff lists the three parts every listing holds.
    let (r, base) = fixture();
    assert_eq!(affected(&r, &base), ["pins", "toolchain", "schema"]);
}
