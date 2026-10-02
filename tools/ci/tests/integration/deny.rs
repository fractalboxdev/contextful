//! `assurance.gate.dependency-deny`: the crate-graph stage runs pinned cargo-deny over each
//! profile's graph of the binary, over scratch workspaces whose offending packages are
//! local stubs named like the crates the refusals deny.

use crate::{manifest, repo_root, stderr, Repo};
use std::process::{Command, Output};

const BINARY: &str = "contextful-cli";

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// `contextful-ci deny` over `root`, its `Cargo.lock` first brought in line with its manifests.
fn deny(r: &Repo) -> Output {
    r.lock();
    Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .arg("deny")
        .current_dir(&r.root)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap()
}

/// A stub package at `stubs/<name>` depending on `deps`, with `features`.
fn stub(r: &Repo, name: &str, deps: &str, features: &str) {
    r.write(
        &format!("stubs/{name}/Cargo.toml"),
        &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}\n[features]\n{features}\n"),
    );
    r.write(&format!("stubs/{name}/src/lib.rs"), "");
}

/// The binary at `crates/contextful-cli`, depending on `deps` and declaring `features`.
fn binary(r: &Repo, deps: &str, features: &str) {
    r.write(&format!("crates/{BINARY}/Cargo.toml"), &format!("{}\n[features]\n{features}", manifest(BINARY, deps)));
    r.write(&format!("crates/{BINARY}/src/lib.rs"), "");
    r.write(&format!("crates/{BINARY}/tests/integration/main.rs"), "");
}

/// A scratch workspace carrying this repository's `deny.toml`, a CRDT consumer stub and a
/// binary whose `control` profile alone links it.
fn workspace() -> Repo {
    let r = Repo::init();
    r.write("deny.toml", &std::fs::read_to_string(repo_root().join("deny.toml")).unwrap());
    stub(&r, "loro", "", "");
    stub(&r, "crdtkit", "loro = { path = \"../loro\" }\n", "");
    r
}

const OPTIONAL_CRDT: &str = "crdtkit = { path = \"../../stubs/crdtkit\", optional = true }\n";

fn refused(o: &Output, error: &str) -> String {
    assert!(!o.status.success(), "expected {error}, got success: {}{}", stdout(o), stderr(o));
    let err = stderr(o);
    assert!(err.contains(error), "expected {error}, got: {err}");
    err
}

fn passes(o: &Output) -> String {
    assert!(o.status.success(), "{}{}", stdout(o), stderr(o));
    stdout(o)
}

/// The crate-graph stage runs pinned cargo-deny `check bans` over each profile's resolved graph, denying every crate a profile-wide dependency refusal names; a hit raises that refusal's error, naming the profile and the path.
// spec: assurance.gate.dependency-deny@586105f4
#[test]
fn the_crate_graph_stage_runs_cargo_deny_over_each_profile() {
    let r = workspace();
    binary(&r, OPTIONAL_CRDT, "contextful-edge = []\ncontextful-full = []\ncontextful-control = [\"dep:crdtkit\"]\n");
    r.lock();
    r.commit("profiles");
    let o = r.gate(&["--stage", "crate-graph"]);
    let out = passes(&o);
    for profile in ["contextful-edge", "contextful-full", "contextful-control"] {
        assert!(out.contains(&format!("dependency deny: profile `{profile}` holds")), "{out}");
    }

    // A script runtime reached from one profile raises the refusal `topology.compose` owns.
    stub(&r, "rquickjs", "", "");
    binary(
        &r,
        &format!("{OPTIONAL_CRDT}rquickjs = {{ path = \"../../stubs/rquickjs\", optional = true }}\n"),
        "contextful-edge = [\"dep:rquickjs\"]\ncontextful-full = []\ncontextful-control = [\"dep:crdtkit\"]\n",
    );
    r.lock();
    r.commit("a script runtime in edge");
    let err = refused(&r.gate(&["--stage", "crate-graph"]), "ScriptRuntimeLinked");
    assert!(err.contains("profile `contextful-edge` links `rquickjs` through contextful-cli -> rquickjs"), "{err}");
    assert!(!err.contains("profile `contextful-full`"), "{err}");
}

/// The CRDT library in the resolved dependency graph of the edge or full profile raises `ProfileDependencyLeak`, naming the profile and the path that pulled it. A daemon or replica reads materialized text.
#[test]
fn the_crdt_library_in_the_edge_or_full_profile_is_refused() {
    let r = workspace();
    binary(&r, OPTIONAL_CRDT, "contextful-edge = []\ncontextful-full = [\"dep:crdtkit\"]\ncontextful-control = [\"dep:crdtkit\"]\n");
    let err = refused(&deny(&r), "ProfileDependencyLeak");
    assert!(err.contains("profile `contextful-full` links `loro` through contextful-cli -> crdtkit -> loro"), "{err}");
    assert!(!err.contains("profile `contextful-control`"), "control is the one profile linking the CRDT library: {err}");

    binary(&r, OPTIONAL_CRDT, "contextful-edge = [\"dep:crdtkit\"]\ncontextful-full = []\ncontextful-control = [\"dep:crdtkit\"]\n");
    let err = refused(&deny(&r), "ProfileDependencyLeak");
    assert!(err.contains("profile `contextful-edge` links `loro` through contextful-cli -> crdtkit -> loro"), "{err}");
}

/// The connector authoring dependency allowlist carries `regex` and `serde_json` at its depth limit; a profile graph reaching `fancy-regex`, `pcre2`, `onig` or `serde_json`'s `unbounded_depth` feature raises `DependencyAllowlistViolation`, naming the profile and path.
// spec: assurance.build.dependency-allowlist@63aca79c
#[test]
fn a_backtracking_matcher_or_unbounded_parser_in_a_profile_is_refused() {
    let r = workspace();
    stub(&r, "regex", "", "");
    stub(&r, "serde_json", "", "unbounded_depth = []\n");
    stub(&r, "fancy-regex", "regex = { path = \"../regex\" }\n", "");
    binary(
        &r,
        &format!("{OPTIONAL_CRDT}regex = {{ path = \"../../stubs/regex\" }}\nserde_json = {{ path = \"../../stubs/serde_json\" }}\n"),
        "contextful-edge = []\ncontextful-full = []\ncontextful-control = [\"dep:crdtkit\"]\n",
    );
    passes(&deny(&r));

    stub(&r, "matchers", "fancy-regex = { path = \"../fancy-regex\" }\n", "");
    binary(
        &r,
        &format!(
            "{OPTIONAL_CRDT}matchers = {{ path = \"../../stubs/matchers\", optional = true }}\nserde_json = {{ path = \"../../stubs/serde_json\" }}\n"
        ),
        "contextful-edge = []\ncontextful-full = [\"dep:matchers\", \"serde_json/unbounded_depth\"]\ncontextful-control = [\"dep:crdtkit\"]\n",
    );
    let err = refused(&deny(&r), "DependencyAllowlistViolation");
    assert!(err.contains("profile `contextful-full` links `fancy-regex` through contextful-cli -> matchers -> fancy-regex"), "{err}");
    assert!(err.contains("profile `contextful-full` turns on `serde_json` feature `unbounded_depth` through contextful-cli -> serde_json"), "{err}");
    assert!(!err.contains("profile `contextful-edge`"), "{err}");
}

/// `deny.toml` holds one `[bans]` entry per crate or feature a profile-wide dependency refusal names, its reason that refusal's clause id; an entry missing or extra raises `DenyListDrift`, naming it.
// spec: assurance.gate.deny-list@39fec38e
#[test]
fn a_deny_list_drifting_from_the_refusals_is_refused() {
    let r = workspace();
    binary(&r, OPTIONAL_CRDT, "contextful-edge = []\ncontextful-full = []\ncontextful-control = [\"dep:crdtkit\"]\n");
    passes(&deny(&r));

    let committed = std::fs::read_to_string(repo_root().join("deny.toml")).unwrap();
    let entry = "{ crate = \"loro\", reason = \"topology.package.crdt-leak\" },";
    assert!(committed.contains(entry), "{committed}");
    r.write("deny.toml", &committed.replace(entry, "{ crate = \"leftpad\", reason = \"topology.package.crdt-leak\" },"));
    let err = refused(&deny(&r), "DenyListDrift");
    assert!(err.contains("`deny.toml` denies no `loro` for `topology.package.crdt-leak`"), "{err}");
    assert!(err.contains("`deny.toml` denies `leftpad`, which no profile-wide dependency refusal names"), "{err}");

    std::fs::remove_file(r.root.join("deny.toml")).unwrap();
    refused(&deny(&r), "DenyListDrift");
}

/// A profile's graph is the binary resolved with default features off and the profile's feature on, across every target, excluding development dependencies; the stage prints each profile the binary declares no feature for.
// spec: assurance.gate.deny-profile-graph@c59b9217
#[test]
fn a_profile_graph_is_the_binary_under_its_feature_alone() {
    let r = workspace();
    // The CRDT library under a default feature, behind another target's `cfg`, and as a
    // development dependency: none is in the edge graph, and only the second is in full.
    stub(&r, "crdt-cfg", "", "");
    r.write("stubs/crdt-cfg/Cargo.toml", "[package]\nname = \"crdt-cfg\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[target.'cfg(target_os = \"none\")'.dependencies]\nloro = { path = \"../loro\" }\n");
    let deps = format!("{OPTIONAL_CRDT}crdt-cfg = {{ path = \"../../stubs/crdt-cfg\", optional = true }}\n");
    r.write(
        &format!("crates/{BINARY}/Cargo.toml"),
        &format!(
            "{}\n[features]\ndefault = [\"dep:crdtkit\"]\ncontextful-edge = []\ncontextful-full = [\"dep:crdt-cfg\"]\n\n[dev-dependencies]\nloro = {{ path = \"../../stubs/loro\" }}\n",
            manifest(BINARY, &deps)
        ),
    );
    r.write(&format!("crates/{BINARY}/src/lib.rs"), "");
    r.write(&format!("crates/{BINARY}/tests/integration/main.rs"), "");
    let o = deny(&r);
    let err = refused(&o, "ProfileDependencyLeak");
    assert!(err.contains("profile `contextful-full` links `loro` through contextful-cli -> crdt-cfg -> loro"), "{err}");
    assert!(!err.contains("profile `contextful-edge`"), "{err}");
    let out = stdout(&o);
    assert!(out.contains("dependency deny: the binary declares no `contextful-control` feature; profile `contextful-control` has no graph"), "{out}");
}

/// This repository's deny list matches its refusals, and every profile the binary declares
/// holds to it.
#[test]
fn this_repository_holds_every_profile_to_its_deny_list() {
    let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .arg("deny")
        .current_dir(repo_root())
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    let out = passes(&o);
    assert!(out.contains("dependency deny: profile `contextful-edge` holds"), "{out}");
    assert!(out.contains("dependency deny: profile `contextful-full` holds"), "{out}");
}
