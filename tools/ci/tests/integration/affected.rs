//! `assurance.gate.affected-*`: `contextful-ci stages --parts --base <rev> --affected` prints
//! the required parts the change reaches, in run order.

use crate::{manifest, stderr, Repo};

const LOCK: &str = "spec/spec.lock.json";

/// The fixture's clauses: each pinned once, by the artifact its subject names.
const CLAUSES: [&str; 7] = [
    "assurance.gate.tagged",
    "assurance.gate.listed",
    "assurance.gate.proved",
    "assurance.gate.connected",
    "assurance.gate.accepted",
    "assurance.gate.surfaced",
    "assurance.gate.modelled",
];

/// The fixture's lock, `reworded` carrying a changed statement and `removed` absent.
fn lock_with(reworded: &[&str], removed: &[&str]) -> String {
    let items: Vec<String> = CLAUSES
        .iter()
        .filter(|id| !removed.contains(id))
        .map(|id| {
            let statement = if reworded.contains(id) { "Reworded." } else { "Stated." };
            format!(r#"{{"id":"{id}","kind":"behavior","statement":"{statement}"}}"#)
        })
        .collect();
    format!(r#"{{"clauses":[{}]}}"#, items.join(","))
}

fn lock(reworded: &[&str]) -> String {
    lock_with(reworded, &[])
}

/// A workspace holding `demo`; the binary with one listed feature run and the edge profile;
/// `contextful-connectors`, a featured connector package; the acceptance package; and a
/// lock of seven clauses, pinned by a tag in `demo`'s test, a pins entry naming the binary's
/// test, a Lean tag under `formal/`, tags in the connector and acceptance tests, a tag in a
/// TypeScript test and a Lean tag outside `formal/`.
fn fixture() -> (Repo, String) {
    let r = Repo::init();
    r.write(
        "crates/contextful-cli/Cargo.toml",
        &format!(
            "{}\n[features]\nextra = []\ncontextful-edge = []\n\n[package.metadata.contextful]\nfeature-runs = [\"extra\"]\n",
            manifest("contextful-cli", "")
        ),
    );
    r.write("crates/contextful-cli/src/lib.rs", "");
    r.write("crates/contextful-cli/tests/integration/main.rs", "#[test]\nfn builds() {}\n");
    r.write("crates/contextful-connectors/Cargo.toml", &format!("{}\n[features]\ndrive = []\n", manifest("contextful-connectors", "")));
    r.write("crates/contextful-connectors/src/lib.rs", "");
    r.write("crates/contextful-connectors/tests/integration/main.rs", "// spec: assurance.gate.connected@00000000\n#[test]\nfn connects() {}\n");
    r.write("crates/acceptance/Cargo.toml", &manifest("contextful-acceptance", ""));
    r.write("crates/acceptance/src/lib.rs", "");
    r.write("crates/acceptance/tests/integration/main.rs", "// spec: assurance.gate.accepted@00000000\n#[test]\nfn accepts() {}\n");
    r.write(
        "crates/demo/tests/integration/double.rs",
        "// spec: assurance.gate.tagged@00000000\n#[test]\nfn doubles() {\n    assert_eq!(demo::double(2), 4);\n}\n",
    );
    r.write("apps/web/test/index.test.ts", "// spec: assurance.gate.surfaced@00000000\ntest(\"surfaces\", () => {});\n");
    r.write("models/Model.lean", "-- spec: assurance.gate.modelled@00000000\ntheorem m : True := trivial\n");
    r.write("spec/pins.toml", "[pin]\n\"assurance.gate.listed\" = { test = \"contextful_cli::builds\" }\n");
    r.write("formal/Demo.lean", "-- spec: assurance.gate.proved@00000000\ntheorem t : True := trivial\n");
    r.write("formal/protocol/lean-toolchain", "leanprover/lean4:v4.0.0\n");
    r.write("formal/reference/lean-toolchain", "leanprover/lean4:v4.0.0\n");
    r.write("spec/81-engineering.md", "# gate\n");
    r.write(LOCK, &lock(&[]));
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

/// The listing after one commit writing `files` onto the fixture, checked as a subset.
fn after(files: &[(&str, &str)]) -> Vec<String> {
    let (r, base) = fixture();
    for (path, text) in files {
        r.write(path, text);
    }
    r.commit("change");
    let got = affected(&r, &base);
    assert_subset(&r, &base, &got);
    got
}

/// A change under `apps/` or `packages/` reaches the TypeScript surfaces, acceptance and crate-graph parts.
// spec: assurance.gate.affected-surfaces@9b5f23f1
#[test]
fn a_typescript_change_reaches_the_surfaces_acceptance_and_crate_graph_parts() {
    let want = ["pins", "toolchain", "schema", "acceptance", "crate-graph", "surfaces"];
    assert_eq!(after(&[("apps/web/src/index.ts", "export const x = 1;\n")]), want);
    assert_eq!(after(&[("packages/kit/src/index.ts", "export const y = 2;\n")]), want);
    assert_eq!(after(&[("apps/console/package.json", "{}\n")]), want);
}

/// A change under `spec/` or `references/` reaches test-first validation, one under `spec/` also the foundation workspace part, and each clause whose statement or `spec/pins.toml` entry changed the parts running its pinned test or theorem.
// spec: assurance.gate.affected-spec@a5da2e1a
#[test]
fn a_spec_change_reaches_validation_and_the_parts_running_each_changed_clause_pin() {
    assert_eq!(after(&[("references/gate.md", "# Gate practice\n")]), ["pins", "toolchain", "schema", "test-first.validate"]);
    assert_eq!(after(&[("spec/guide/gate.md", "# The gate\n")]), ["pins", "toolchain", "schema", "test-first.validate", "workspace.foundation"]);

    // A reworded clause pinned by a tag in `demo`'s test reaches the parts running `demo`.
    assert_eq!(
        after(&[(LOCK, &lock(&["assurance.gate.tagged"]))]),
        ["pins", "toolchain", "schema", "test-first.validate", "workspace.compile", "workspace.foundation"]
    );

    // A clause pinned in `spec/pins.toml` to the binary's test reaches every binary part.
    let got = after(&[(LOCK, &lock(&["assurance.gate.listed"]))]);
    for part in ["workspace.compile", "workspace.cli-1", "workspace.cli-4", "workspace.cli-formal", "features.binary-extra", "features.formal-extra"] {
        assert!(got.iter().any(|p| p == part), "`{part}` missing: {got:?}");
    }
    assert!(!got.iter().any(|p| p == "surfaces" || p == "formal" || p == "connectors"), "{got:?}");

    // A clause a Lean theorem pins, under `formal/` or outside it, reaches the formal parts.
    for id in ["assurance.gate.proved", "assurance.gate.modelled"] {
        let got = after(&[(LOCK, &lock(&[id]))]);
        assert!(got.iter().any(|p| p == "formal") && got.iter().any(|p| p == "workspace.cli-formal"), "{id}: {got:?}");
        assert!(!got.iter().any(|p| p == "workspace.cli-1"), "{id}: {got:?}");
    }

    // A changed `spec/pins.toml` entry reaches the parts running the test it now names.
    let got = after(&[("spec/pins.toml", "[pin]\n\"assurance.gate.listed\" = { test = \"crates/demo/tests/integration/double.rs::doubles\" }\n")]);
    for part in ["workspace.foundation", "workspace.cli-1"] {
        assert!(got.iter().any(|p| p == part), "`{part}` missing, the old and new pins both reach: {got:?}");
    }
}

/// A clause pinned in a package's test reaches the parts running that package: a featured package its own feature part, a connector package the connectors part, the acceptance package the acceptance part, and a TypeScript test the surfaces part
/// (`assurance.gate.affected-spec`).
#[test]
fn a_changed_clause_reaches_the_parts_running_its_package() {
    assert_eq!(
        after(&[(LOCK, &lock(&["assurance.gate.connected"]))]),
        ["pins", "toolchain", "schema", "test-first.validate", "workspace.compile", "workspace.foundation", "workspace.runtime", "features.connectors", "connectors"]
    );
    assert_eq!(
        after(&[(LOCK, &lock(&["assurance.gate.accepted"]))]),
        ["pins", "toolchain", "schema", "test-first.validate", "workspace.foundation", "acceptance"]
    );
    assert_eq!(
        after(&[(LOCK, &lock(&["assurance.gate.surfaced"]))]),
        ["pins", "toolchain", "schema", "test-first.validate", "workspace.foundation", "surfaces"]
    );
}

/// A change to `spec/01-topology.md` or `spec/capabilities.toml` reaches crate-graph, to `spec/adr/A-assurance.md` the formal stage, and to `spec/terms/assurance.toml` every budget part.
// spec: assurance.gate.affected-spec-inputs@1e096c26
#[test]
fn a_spec_file_a_stage_reads_reaches_that_stage() {
    fn with(extra: &'static str) -> Vec<&'static str> {
        vec!["pins", "toolchain", "schema", "test-first.validate", "workspace.foundation", extra]
    }
    let topology = with("crate-graph");
    assert_eq!(after(&[("spec/01-topology.md", "# Topology\n")]), topology);
    assert_eq!(after(&[("spec/capabilities.toml", "[capability]\n")]), topology);
    assert_eq!(after(&[("spec/adr/A-assurance.md", "# A-assurance\n")]), with("formal"));
    assert_eq!(after(&[("spec/terms/assurance.toml", "[error]\n")]), with("budget.edge"));
}

/// A clause the merge base's lock holds and HEAD's lacks reaches the evaluate stage.
// spec: assurance.gate.affected-removed@71546334
#[test]
fn a_removed_clause_reaches_the_evaluate_stage() {
    let got = after(&[(LOCK, &lock_with(&[], &["assurance.gate.tagged"]))]);
    for part in ["evaluate.foundation", "evaluate.runtime", "evaluate.read", "evaluate.cli", "evaluate.ci", "evaluate.probe"] {
        assert!(got.iter().any(|p| p == part), "`{part}` missing: {got:?}");
    }
    let got = after(&[(LOCK, &lock(&["assurance.gate.tagged"]))]);
    assert!(!got.iter().any(|p| p.starts_with("evaluate.")), "a reworded clause keeps its ledger entry resolved: {got:?}");
}

/// A change under `formal/` other than a `lean-toolchain` file reaches the formal and evaluate stages, the foundation, CLI and CLI differential workspace parts, and every feature part.
// spec: assurance.gate.affected-formal@bb4475ac
#[test]
fn a_formal_change_reaches_every_part_reading_the_lean_tree() {
    let got = after(&[("formal/Demo.lean", "-- spec: assurance.gate.proved@00000000\ntheorem t : True := by trivial\n")]);
    assert_eq!(
        got,
        [
            "pins",
            "toolchain",
            "schema",
            "workspace.cli-1",
            "workspace.cli-2",
            "workspace.cli-3",
            "workspace.cli-4",
            "workspace.cli-formal",
            "workspace.foundation",
            "evaluate.foundation",
            "evaluate.runtime",
            "evaluate.read",
            "evaluate.cli",
            "evaluate.ci",
            "evaluate.probe",
            "features.connectors",
            "features.binary-none",
            "features.formal-none",
            "features.binary-all",
            "features.formal-all",
            "features.binary-extra",
            "features.formal-extra",
            "formal",
        ]
    );
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
        "formal/protocol/lean-toolchain",
        "formal/reference/lean-toolchain",
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
    // A spec lock the listing cannot read at HEAD reaches every part.
    let (r, base) = fixture();
    r.write(LOCK, "{ not json");
    r.commit("unreadable lock");
    assert_eq!(affected(&r, &base), required(&r, &base));

    // A spec lock the listing cannot read at the merge base reaches every part.
    let (r, _) = fixture();
    r.write(LOCK, "{ not json");
    let base = r.commit("unreadable lock");
    r.write(LOCK, &lock(&[]));
    r.commit("readable lock");
    assert_eq!(affected(&r, &base), required(&r, &base));

    // A changed clause pinned to a test whose package the workspace does not hold reaches every part.
    let (r, base) = fixture();
    r.write("spec/pins.toml", "[pin]\n\"assurance.gate.listed\" = { test = \"contextful_absent::gone\" }\n");
    r.commit("pin outside the workspace");
    assert_eq!(affected(&r, &base), required(&r, &base));

    // A changed pin naming neither a test, an item nor a theorem reaches every part.
    let (r, base) = fixture();
    r.write("spec/pins.toml", "[pin]\n\"assurance.gate.listed\" = { note = \"unplaced\" }\n");
    r.commit("pin naming nothing");
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
    r.write("references/gate.md", "# Gate practice\n");
    r.commit("prose only");
    r.git(&["checkout", "-q", "main"]);
    r.write("crates/demo/src/lib.rs", "pub fn double(x: i32) -> i32 {\n    x + x\n}\n");
    let main = r.commit("main moves on");
    r.git(&["checkout", "-q", "topic"]);
    assert_ne!(main, base);
    // The base gained a source change after the branch; the listing measures the branch alone.
    assert_eq!(affected(&r, &main), ["pins", "toolchain", "schema", "test-first.validate"]);

    // An empty diff lists the three parts every listing holds.
    let (r, base) = fixture();
    assert_eq!(affected(&r, &base), ["pins", "toolchain", "schema"]);
}
